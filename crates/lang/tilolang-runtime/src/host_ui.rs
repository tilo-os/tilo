//! Imports that build a view's UI tree. Views only.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_f64, ret_i32, ret_i64, ret_void, trap};
use crate::engine::HostFail;
use crate::engine::Wasm;
use crate::handles::{FrameKind, ViewState};
use crate::render;
use crate::space::Space;
use crate::value::{ActionRef, Value};
use crate::view::{BindTarget, Binding, NodeBuild, UiNode};
use crate::view_state::StateKey;

/// The action id of `set(name, value)`: changes a view state slot. Its arguments are the
/// window, the person, the declaration id and the value.
pub const SET_ACTION: &str = "$set";
/// The action the host wires to the selection of a list it keeps: what the list reports of it
/// goes in the window's own slot, the runtime's reading rather than a state the person declared.
/// Its arguments are the slot's id and the value reported.
pub const KEEP_ACTION: &str = "$keep";
/// The action id of `open(r, view: v, front: f)`: its arguments are the resource, the view id
/// (empty for the default view of its type) and whether a narrow window brings it in front.
pub const OPEN_ACTION: &str = "$open";

/// The action of a pressed case of an enum field: the field the case belongs to takes it. Its
/// argument is the case's name; the press puts the node the case stands in before it, since the
/// key a window draws a case by is the press's, not the view's.
pub const CHOOSE_CASE_ACTION: &str = "$choose-case";

/// The action of a handler that binds an action's answer (`let id = add_box(b)`): its arguments
/// are the action, the id of the function of the steps after the binding, and the values those
/// steps are handed besides the answer. The steps run when the action answers, and not when it
/// is refused or fails.
pub const ANSWER_ACTION: &str = "$answer";

/// The system's Copy link on a resource, which every menu of one offers: the window's host mints
/// the link other devices and people open it by and puts it on the person's clipboard. Its
/// arguments are the resource and the app whose resource it is.
pub const COPY_LINK_ACTION: &str = "$copy-link";

/// The system verb `id`, an action of a resource's type, names (`AgentCard/share-link`,
/// `so.tilo.notes#Note/share-link`): one the system answers on every resource, never an app's own
/// action. `None` for an app's action, whose id names no system verb.
#[must_use]
pub fn system_verb(id: &str) -> Option<&str> {
    let (_, verb) = id.rsplit_once('/')?;
    [tilo_actions::remote::SHARE_LINK_ACTION]
        .contains(&verb)
        .then_some(verb)
}

/// The system's Export of a resource, which every menu of one offers: the window's host saves it
/// as a file where the person says ([`crate::export`]). Its arguments are the resource and the
/// app whose resource it is.
pub const EXPORT_ACTION: &str = "$export";

/// The action of a row's keys (`shortcut` on a `ui.ListItem`): run the action of the row's item
/// the key pressed names, where it applies. Its arguments are the keys, each with what it runs,
/// and the key pressed.
pub const SHORTCUT_ACTION: &str = "$shortcut";

/// The action of `file.choose(a)`: ask the person for a file, and run `a` with it as the
/// argument it still needs. Its arguments are `a` and the media types it may be. A parameter of
/// `List<File>` takes every file the person chose.
pub const CHOOSE_ACTION: &str = "$choose";

/// The action of `file.choose(a, edit: true)`: ask the person for a file, opened for writing as
/// well as reading, and run `a` with it — a document app's Open, so the document saves back where
/// it was read. Its arguments are `a` and the media types of the files kept.
pub const CHOOSE_EDITING_ACTION: &str = "$choose-editing";

/// The action of `file.choose_folder(a)`: ask the person for a folder, and run `a` with the
/// files it holds, each with its path inside the folder, as the argument it still needs. Its
/// arguments are `a` and the media types of the files kept.
pub const CHOOSE_FOLDER_ACTION: &str = "$choose-folder";

/// The action of `file.save(a, name: n)`: run `a`, and put what it made — text, or a file —
/// where the person says, under the name `n`. Its arguments are `a` and the name.
pub const SAVE_ACTION: &str = "$save";

/// The action of `blur()`: the field the keyboard is in ends its edit, as closing its editor
/// does. The host tells the window through [`crate::ViewEvent::EndEdit`].
pub const BLUR_ACTION: &str = "$blur";

/// The action of `close()`: the window the view stands in closes, as the person closing it does.
/// The host tells the window through [`crate::ViewEvent::CloseWindow`].
pub const CLOSE_ACTION: &str = "$close";

/// The action of `join_call(r)` and its kin: a step of the person's own in the system's call held
/// on `r`. Its arguments are `r`, the step's name (`join`, `leave`, `mute`, `deafen`, `camera`,
/// `screen`) and whether it turns its switch on.
pub const CALL_ACTION: &str = "$call";

/// The action of `select_call_input(r, id)`: send this device's microphone in the system's call
/// held on `r` from the input `id`. Its arguments are `r` and the input's id.
pub const CALL_INPUT_ACTION: &str = "$call-input";

/// The action of `set_call_volume(r, member, percent)`: play `member` in the system's call held
/// on `r` at `percent` of as they arrive, on this device only. Its arguments are `r`, the person
/// and the percent.
pub const CALL_VOLUME_ACTION: &str = "$call-volume";

/// The loudest a member is played in a call, in percent of as they arrive: the call service's
/// own bound (`tilo_actions::remote::native_media::MAX_VOLUME_PERCENT`), which a step beyond is
/// refused by.
pub const CALL_MAX_VOLUME: u16 = 200;

/// A call's volume as a step names it, whole and within the call service's loudest: the percent
/// of as a member arrives, rounded, or why it is not one.
pub(crate) fn call_volume_percent(percent: f64) -> Result<u16, &'static str> {
    if !percent.is_finite() || !(0.0..=f64::from(CALL_MAX_VOLUME)).contains(&percent) {
        return Err("a call's volume is between 0 and 200 percent of as they arrive");
    }
    // Rounded to whole percent, which the range check above keeps within the call service's own
    // bound: the whole number is read without a float cast, and refused rather than wrapped.
    u16::try_from(crate::num::round_whole(percent))
        .ok()
        .filter(|whole| *whole <= CALL_MAX_VOLUME)
        .ok_or("a call's volume is between 0 and 200 percent of as they arrive")
}

/// The action of `run_line(r, line)`: run a tilolang line as the person's own step, from the
/// resource `r`. Its arguments are `r` and the line.
pub const RUN_LINE_ACTION: &str = "$run-line";

/// The prefix of the view state slot that names the view a window shows a resource through,
/// the resource's id after it: what the bar's view switch sets. Empty until a person picks a
/// view, so the resource's type's default view shows until then.
pub const VIEW_STATE: &str = "$view:";

/// The action of a change mark (`ui.ChangeMark`, and those the host places): show what the
/// run that made the change did. Its arguments are the run (its group) and the resource.
pub const REVIEW_ACTION: &str = "$review";

/// The action of `undo(v)` and `undo(run)`: take the step back, or every step of the run,
/// through the platform's history. Its arguments are what it takes back (`step` or `run`), the
/// change or the run, and the resource the version is of (`Unit` for a run).
pub const UNDO_ACTION: &str = "$undo";

/// The action of a catch-up's *Mark all seen*: every change the person has not seen of the
/// resources it lists, in one move. Its only argument is the list of their ids; what it moved
/// can be given back with [`crate::Runtime::undo_seen`].
pub const MARK_SEEN_ACTION: &str = "$mark-seen";

impl Core {
    pub(crate) fn with_view<R>(
        &self,
        f: impl FnOnce(&mut ViewState) -> Result<R, String>,
    ) -> HResult<R> {
        let mut frames = self.frames.borrow_mut();
        match &mut frames.top_mut()?.kind {
            FrameKind::View(vs) => {
                let kept = f(vs)?;
                // What the node was told to show while the view's own state was borrowed for
                // the build is the view's read too.
                vs.person_reads |= self.person_shown.take();
                Ok(kept)
            }
            _ => trap("ui nodes can only be built inside a view"),
        }
    }

    /// Do `f` with the state of the view being drawn, when the code now running draws one: what
    /// it read is a dependency of a boundary only where a view reads it. Code outside a view (an
    /// action's, an agent's program) reads the host's answers without depending on them.
    pub(crate) fn in_view(&self, f: impl FnOnce(&mut ViewState)) {
        let Ok(mut frames) = self.frames.try_borrow_mut() else {
            // The view is building one of its own nodes: what the node shows is taken onto the
            // view when the build is done ([`Self::with_view`]).
            self.person_shown.set(true);
            return;
        };
        if let Ok(top) = frames.top_mut()
            && let FrameKind::View(vs) = &mut top.kind
        {
            f(vs);
        }
    }

    fn node_index(&self, h: i32) -> HResult<usize> {
        self.with_val(h, |v| match v {
            Value::Node(i) => Ok(*i),
            other => trap(format!("expected a ui node, got {}", other.kind_name())),
        })
    }

    fn set_prop(&self, node: i32, name: i32, v: Value) -> HResult<Option<Wasm>> {
        let (at, name) = (self.node_index(node)?, self.text(name)?);
        // A view's call keeps its arguments as `arg.<name>` properties, which the runtime reads
        // and hands the view itself, so a record travels there: it never reaches the window. Any
        // other property is the window's, and a node or a record has no form on the wire.
        let call = self.with_view(|vs| {
            Ok(vs.nodes.get(at).is_some_and(|node| {
                node.kind.starts_with(render::FOR_EACH)
                    || node.kind.starts_with(render::FOREIGN_VIEW)
            }))
        })?;
        let argument = call && name.starts_with(render::ARG_PREFIX);
        if matches!(v, Value::Node(_)) || matches!(v, Value::Rec(_)) && !argument {
            return trap("a ui node or record cannot be a property; use ui_child");
        }
        self.with_view(|vs| {
            let n = vs.nodes.get_mut(at).ok_or("unknown ui node")?;
            n.props.insert(name, v);
            Ok(())
        })?;
        ret_void()
    }

    /// `ui.MenuButton(actions: actions(r))`: the prop `name` of `node` holds each action of the
    /// list `list` (`host.Action`s) as its control shows it, and the node's event
    /// `<name>.<index>` runs it, as an event the view gave it would.
    fn offer_actions(&self, node: i32, name: i32, list: i32) -> HResult<Option<Wasm>> {
        let (at, name) = (self.node_index(node)?, self.text(name)?);
        let Value::List(actions) = self.val_whole(list)? else {
            return trap("offered actions are a list");
        };
        let mut offered = Vec::with_capacity(actions.len());
        let mut events = Vec::with_capacity(actions.len());
        for (index, action) in actions.into_iter().enumerate() {
            let event = offered_event(&name, index);
            let (shown, run) = offered_action(&action, &event)?;
            offered.push(shown);
            events.push((event, run));
        }
        self.with_view(|vs| {
            let n = vs.nodes.get_mut(at).ok_or("unknown ui node")?;
            n.props.insert(name, Value::List(offered));
            n.events.extend(events);
            Ok(())
        })?;
        ret_void()
    }

    /// A prop `name` of `node` that is one offered action (`ui.NavSection(add: g.add)`): an
    /// action, or nothing, as `host.Action` says of it; the node's event `name` runs it. An
    /// action the view names (`add: new_project(t)`) is shown by what it declares.
    fn offer_action(&self, node: i32, name: i32, value: i32) -> HResult<Option<Wasm>> {
        let (at, name) = (self.node_index(node)?, self.text(name)?);
        let offered = match self.val_whole(value)? {
            Value::Opt(None) => return ret_void(),
            Value::Opt(Some(held)) => (*held).clone(),
            other => other,
        };
        let (shown, run) = match &offered {
            Value::Action(reference) => {
                let action = self.action_ref_value(reference)?;
                offered_action(&action, &offered_event(&name, 0))?
            }
            other => offered_action(other, &offered_event(&name, 0))?,
        };
        self.with_view(|vs| {
            let n = vs.nodes.get_mut(at).ok_or("unknown ui node")?;
            n.props.insert(name.clone(), shown);
            // A single action is one the component reads by its prop, and its event is the
            // first of the prop's, as a control that offers a list of actions reports one.
            n.events.insert(offered_event(&name, 0), run);
            Ok(())
        })?;
        ret_void()
    }

    /// The slot a `state` declaration of the running view names.
    fn state_key(&self, id: i32) -> HResult<StateKey> {
        let state = self.text(id)?;
        self.with_view(|vs| Ok(StateKey::view(vs.window.clone(), vs.viewer.clone(), state)))
    }

    /// `state_shown(address, state)`: what the window showing `address` keeps of its declared
    /// `state`. Nothing until a window keeps one; the host keeps the answer as it moves.
    fn state_shown(&self, address_h: i32, state_h: i32) -> HResult<Option<Wasm>> {
        let (address, state) = (self.text(address_h)?, self.text(state_h)?);
        let who = self.looking()?;
        // The slots are the person's own: an agent acting for them reads them through its
        // grants, never through a view of theirs.
        if !matches!(who.member_id(), Some(crate::ids::MemberId::Person(_))) {
            return trap(
                "another window's state is the person's own, not a caller's acting for them",
            );
        }
        let key = StateKey::shown(&address, who, state.clone());
        let live = match self.invoker.borrow().as_ref() {
            Some(invoker) => invoker
                .state_shown(&address, &state)
                .map_err(HostFail::Trap)?,
            None => None,
        };
        if let Some(json) = live {
            self.view_states
                .borrow_mut()
                .restore(key.clone(), Value::Json(Box::new(json)));
        }
        let (held, generation) = {
            let states = self.view_states.borrow();
            (states.get(&key).cloned(), states.generation(&key))
        };
        let value = held.map_or_else(Value::none, Value::some);
        self.note_state_read(key, generation, &value);
        self.ret_h(value)
    }

    fn state_get(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let key = self.state_key(a.h(0)?)?;
        let fallback = self.val_whole(a.h(1)?)?;
        let (held, generation) = {
            let states = self.view_states.borrow();
            // What the person set, else what a scenario staged the declaration at, else what
            // its view declares.
            let held = states
                .get(&key)
                .or_else(|| states.seeded(&key))
                .cloned()
                .unwrap_or(fallback);
            (held, states.generation(&key))
        };
        // What the slot names, as the view reads it: a resource the workspace removed is gone.
        let value = self.held_while_here(held);
        let seen = value.clone();
        self.with_view(|vs| {
            vs.state_reads.push((key.clone(), generation));
            vs.state_seen.push((key, seen));
            Ok(())
        })?;
        self.ret_h(value)
    }

    /// `value` as the view reads it while the workspace holds what it names: a state holding a
    /// reference to a resource the workspace removed reads as nothing, so the page a window
    /// drew of it closes rather than drawing a record that is gone — whoever removed it: an
    /// action here or at another surface, an agent, or a change another device made. A
    /// resource that has not reached this device yet is left as it is: it may still come.
    fn held_while_here(&self, value: Value) -> Value {
        let Value::Opt(Some(inner)) = &value else {
            return value;
        };
        let Value::Ref(held) = &**inner else {
            return value;
        };
        if self.removed_here(&held.target) {
            return Value::none();
        }
        value
    }

    /// Whether the workspace removed `target`: the store does not hold it, and its last version
    /// is its removal. A root's history names every item made in it, so it is read only for a
    /// resource the store does not hold.
    fn removed_here(&self, target: &crate::value::ResourceRef) -> bool {
        let store = self.store.borrow();
        matches!(
            store.type_of(target),
            Err(crate::store::StoreError::NotFound(_))
        ) && store.versions(target).is_ok_and(|versions| {
            versions
                .last()
                .is_some_and(|last| last.kind == crate::store::VersionKind::Removed)
        })
    }

    /// The view state slot itself, as an argument of an action reference: it is read when the
    /// event fires ([`Value::State`]).
    fn state_at(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let key = self.state_key(a.h(0)?)?;
        let scalar = a.h(1)? != 0;
        self.ret_h(Value::State { key, scalar })
    }

    /// An argument of an action reference computed from view state, as a function of the
    /// module the runtime runs when the event fires ([`Value::AtEvent`]).
    fn arg_at(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let id = self.text(a.h(0)?)?;
        let Value::List(args) = self.val(a.h(1)?)? else {
            return trap("arg_at expects the arguments as a list");
        };
        let scalar = a.h(2)? != 0;
        let module = self.module_of_frame()?;
        self.ret_h(Value::AtEvent {
            module,
            id,
            args,
            scalar,
        })
    }

    /// `bind: name`, or with `at` `bind: name.at(i)`: the place `at` of the list the state holds.
    fn bind_state(&self, a: &Args<'_>, at: Option<usize>) -> HResult<Option<Wasm>> {
        let node = self.node_index(a.h(0)?)?;
        let key = self.state_key(a.h(1)?)?;
        let (name, ty) = self
            .registry
            .borrow()
            .state_meta(&key.state)
            .map(|s| (s.name, s.ty))
            .map_or_else(
                || trap(format!("no view declares the state `{}`", key.state)),
                Ok,
            )?;
        let binding = Binding {
            target: BindTarget::State { key, name, ty, at },
            author: self.module_of_frame()?,
        };
        self.with_view(|vs| {
            vs.nodes.get_mut(node).ok_or("unknown ui node")?.binding = Some(binding);
            Ok(())
        })?;
        ret_void()
    }

    /// `keep(id, value)`: the action that puts `value` in the window's `kept` state `id` — the
    /// platform's reading of where the person is, never an entry in their record.
    fn act_keep(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let state = self.text(a.h(0)?)?;
        let value = self.val_whole(a.h(1)?)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: KEEP_ACTION.to_owned(),
            args: vec![Value::Text(state), value],
            author,
            from: None,
        }))
    }

    /// `kept_state(id, default)`: what the window keeps for the person at `id`, else `default`.
    /// Read at the slot the window's own keeps live in, so a view that read it runs again when
    /// the person moves.
    fn kept_get(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let state = self.text(a.h(0)?)?;
        let fallback = self.val_whole(a.h(1)?)?;
        let (key, generation, held) = self.with_view(|vs| {
            let key = crate::view_state::StateKey::runtime(
                vs.window.clone(),
                Some(vs.viewer.clone()),
                state.clone(),
            );
            let states = self.view_states.borrow();
            let held = states.get(&key).cloned().unwrap_or(fallback);
            let generation = states.generation(&key);
            Ok((key, generation, held))
        })?;
        let value = self.held_while_here(held);
        let seen = value.clone();
        self.with_view(|vs| {
            vs.state_reads.push((key.clone(), generation));
            vs.state_seen.push((key, seen));
            Ok(())
        })?;
        self.ret_h(value)
    }

    fn act_set(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let key = self.state_key(a.h(0)?)?;
        let value = self.val_whole(a.h(1)?)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: SET_ACTION.to_owned(),
            args: vec![
                Value::Text(key.window),
                key.actor
                    .map_or_else(Value::none, |who| Value::some(Value::Actor(who))),
                Value::Text(key.state),
                value,
            ],
            author,
            from: None,
        }))
    }

    /// `a.then(b)`: `a`, and once it is done, `b` (a composer's send, then its entry cleared).
    fn act_then(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let first = self.val(a.h(0)?)?;
        let next = self.val(a.h(1)?)?;
        for v in [&first, &next] {
            if !matches!(v, Value::Action(_)) {
                return trap(format!("`then` joins actions, not {}", v.kind_name()));
            }
        }
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: crate::value::THEN_ACTION.to_owned(),
            args: vec![first, next],
            author,
            from: None,
        }))
    }

    /// `let id = act(args)` in a handler: `act`, and the steps after the binding, run with the
    /// answer once `act` gives one. The steps are the function `id` names, handed the answer and
    /// the values `captures` holds.
    fn act_answer(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let first = self.val(a.h(0)?)?;
        if !matches!(first, Value::Action(_)) {
            return trap(format!(
                "a handler binds the answer of an action, not {}",
                first.kind_name()
            ));
        }
        let id = self.text(a.h(1)?)?;
        let captures = self.val(a.h(2)?)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: ANSWER_ACTION.to_owned(),
            args: vec![first, Value::Text(id), captures],
            author,
            from: None,
        }))
    }

    /// `file.choose(a, accept: [...])`: ask the person for a file and run `a` with it as the
    /// argument it still needs. The chooser is the device's ([`crate::blob::BlobHost`]), and
    /// what it answers is what the action is invoked with: its invocation records the file's
    /// address, so history, replay and undo carry it.
    fn act_choose(&self, a: &Args<'_>, action_name: &str) -> HResult<Option<Wasm>> {
        let action = self.val(a.h(0)?)?;
        if !matches!(action, Value::Action(_)) {
            return trap(format!(
                "`file.choose` takes the action it runs, not {}",
                action.kind_name()
            ));
        }
        let accept: Vec<Value> = match self.val(a.h(1)?)? {
            Value::List(items) => items
                .iter()
                .filter_map(|item| item.as_text().map(Value::text))
                .collect(),
            other => {
                return trap(format!(
                    "`accept` takes the media types, not {}",
                    other.kind_name()
                ));
            }
        };
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: action_name.to_owned(),
            args: vec![action, Value::List(accept)],
            author,
            from: None,
        }))
    }

    /// `file.save(a, name: n, then: b)`: run `a` and put what it made where the person says;
    /// `b`, when given, runs with what the save came to.
    fn act_save(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let action = self.val(a.h(0)?)?;
        if !matches!(action, Value::Action(_)) {
            return trap(format!(
                "`file.save` takes the action it runs, not {}",
                action.kind_name()
            ));
        }
        let name = self.text(a.h(1)?)?;
        let then = match a.h(2) {
            Ok(handle) if handle != 0 => match self.val(handle)? {
                Value::Opt(Some(held)) => Value::Opt(Some(held)),
                Value::Opt(None) => Value::Opt(None),
                Value::Action(aref) => Value::Opt(Some(Box::new(Value::Action(aref)))),
                other => {
                    return trap(format!(
                        "`file.save`'s `then:` takes the action it runs, not {}",
                        other.kind_name()
                    ));
                }
            },
            _ => Value::Opt(None),
        };
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: SAVE_ACTION.to_owned(),
            args: vec![action, Value::Text(name), then],
            author,
            from: None,
        }))
    }

    /// `blur()`: an action that, run, tells the window the field the keyboard is in ends its
    /// edit. Views only.
    fn act_close(&self) -> HResult<Option<Wasm>> {
        self.with_view(|_| Ok(()))?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: CLOSE_ACTION.to_owned(),
            args: Vec::new(),
            author,
            from: None,
        }))
    }

    fn act_blur(&self) -> HResult<Option<Wasm>> {
        self.with_view(|_| Ok(()))?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: BLUR_ACTION.to_owned(),
            args: Vec::new(),
            author,
            from: None,
        }))
    }

    /// `a.made_from(r)`: the action `a`, invoked made from the resource `r`: what the resources
    /// it makes record as what they were made from (`created(r)?.from`), as a block a scenario
    /// runs `made from r { ... }` says of its changes.
    fn act_made_from(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let action = self.val(a.h(0)?)?;
        let Value::Action(action) = action else {
            return trap(format!(
                "`made_from` takes an action, not {}",
                action.kind_name()
            ));
        };
        let from = self.ref_arg(a.h(1)?)?;
        self.ret_h(Value::Action(action.made_from(Some(&from.target))))
    }

    /// `as.many()`: one action that applies each of `as` to its own item, all at once. The
    /// runtime runs them in one transaction, so they are one change: one undo takes them all
    /// back, and history says how many items it changed.
    fn act_many(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let list = self.val(a.h(0)?)?;
        let Value::List(items) = &list else {
            return trap(format!(
                "`.many` takes a list of actions, not {}",
                list.kind_name()
            ));
        };
        for item in items {
            if !matches!(item, Value::Action(_)) {
                return trap(format!("`.many` takes actions, not {}", item.kind_name()));
            }
        }
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: crate::exec::MANY_ACTION.to_owned(),
            args: vec![list],
            author,
            from: None,
        }))
    }

    fn act_open(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        // The resource, or the place of the one an event carries.
        let target = match self.val(a.h(0)?)? {
            Value::Ref(target) => Value::Ref(target),
            Value::Event => Value::Event,
            Value::EventPart(part) => Value::EventPart(part),
            other => {
                return trap(format!(
                    "expected a resource reference, got {}",
                    other.kind_name()
                ));
            }
        };
        let view = self.text(a.h(1)?)?;
        let front = a.h(2)? != 0;
        self.with_view(|_| Ok(()))?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: OPEN_ACTION.to_owned(),
            args: vec![target, Value::Text(view), Value::Bool(front)],
            author,
            from: None,
        }))
    }

    /// `join_call(r)`, `leave_call(r)`, `mute_call(r, on)` and their kin: a step of the person's
    /// own in the system's call held on `r`, which the window's host carries out.
    fn act_call(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let target = match self.val(a.h(0)?)? {
            Value::Ref(target) => Value::Ref(target),
            other => {
                return trap(format!(
                    "a call is held on a resource, not {}",
                    other.kind_name()
                ));
            }
        };
        let step = self.text(a.h(1)?)?;
        let on = a.h(2)? != 0;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: CALL_ACTION.to_owned(),
            args: vec![target, Value::Text(step), Value::Bool(on)],
            author,
            from: None,
        }))
    }

    /// `select_call_input(r, id)`: send this device's microphone in the system's call held on `r`
    /// from the input `id`, which the window's host carries out.
    fn act_call_input(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let target = self.call_target(a.h(0)?)?;
        let input = Value::Text(self.text(a.h(1)?)?);
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: CALL_INPUT_ACTION.to_owned(),
            args: vec![target, input],
            author,
            from: None,
        }))
    }

    /// `set_call_volume(r, member, percent)`: how loud this device plays `member` in the system's
    /// call held on `r`, which the window's host carries out.
    fn act_call_volume(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let target = self.call_target(a.h(0)?)?;
        let member = self.actor_arg(a.h(1)?)?;
        let percent = a.float(2)?;
        let Some(percent) = call_volume_percent(percent).ok() else {
            return trap(
                "a call's volume is between 0 and 200 percent of as they arrive".to_owned(),
            );
        };
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: CALL_VOLUME_ACTION.to_owned(),
            args: vec![
                target,
                Value::Actor(member),
                Value::Float(f64::from(percent)),
            ],
            author,
            from: None,
        }))
    }

    /// The resource `join_call(r)` and its kin name, as the value a call action holds.
    fn call_target(&self, h: i32) -> HResult<Value> {
        match self.val(h)? {
            Value::Ref(target) => Ok(Value::Ref(target)),
            other => trap(format!(
                "a call is held on a resource, not {}",
                other.kind_name()
            )),
        }
    }

    /// `run_line(r, line)`: the person's own tilolang line, run from `r`, which the window's host
    /// carries out through the session's program host.
    fn act_run_line(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let target = match self.val(a.h(0)?)? {
            Value::Ref(target) => Value::Ref(target),
            other => {
                return trap(format!(
                    "a line is run from a resource, not {}",
                    other.kind_name()
                ));
            }
        };
        let line = self.text(a.h(1)?)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: RUN_LINE_ACTION.to_owned(),
            args: vec![target, Value::Text(line)],
            author,
            from: None,
        }))
    }

    /// `undo(v)` of a `host.Version` (`a.h(1) == 0`) or `undo(run)` of a `host.Run`.
    fn act_undo(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let held = self.val(a.h(0)?)?;
        let run = a.h(1)? != 0;
        let Value::Tuple(fields) = held else {
            return trap("undo: expected a version or a run");
        };
        let args = if run {
            let Some(Value::Text(id)) = fields.first() else {
                return trap("undo: a run names its group first");
            };
            vec![
                Value::Text("run".to_owned()),
                Value::Text(id.clone()),
                Value::Unit,
            ]
        } else {
            let of = tilolang::host::value_field("Version", "of").and_then(|at| fields.get(at));
            // The change is held after the action's id, past the fields code reads by name.
            let change = tilolang::host::action_id_at("Version")
                .and_then(|at| fields.get(at.checked_add(1)?));
            let (Some(Value::Text(change)), Some(of)) = (change, of) else {
                return trap("undo: a version names its change and its resource");
            };
            if change.is_empty() {
                return trap("undo: a version not committed yet names no change");
            }
            vec![
                Value::Text("step".to_owned()),
                Value::Text(change.clone()),
                of.clone(),
            ]
        };
        self.with_view(|_| Ok(()))?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: UNDO_ACTION.to_owned(),
            args,
            author,
            from: None,
        }))
    }

    /// `c.before` (`a.h(1) == 0`) or `c.after` of a `host.Change`, keyed `a.h(2)`: a node
    /// showing what the field held as its type shows it.
    fn change_view(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let Value::Tuple(change) = self.val(a.h(0)?)? else {
            return trap("expected a change");
        };
        let side = if a.h(1)? == 0 { "before" } else { "after" };
        let key = self.text(a.h(2)?)?;
        let Some(shown) = tilolang::host::value_field("Change", side)
            .and_then(|at| change.get(at))
            .cloned()
        else {
            return trap("a change holds what it was before and after");
        };
        let phrase = |id: &str| self.word_args(id, &[], id);
        let root = self.with_view(|vs| {
            let root = push_tree(vs, shown_node(&key, &shown, &phrase));
            // What a field held before reads quieter than what it holds now. A rich text
            // cannot take it (its component declares no such prop): its before side says
            // what the change took away by striking those runs.
            if side == "before"
                && let Some(node) = vs.nodes.get_mut(root)
                && node.kind == "ui.Text"
            {
                node.props.insert("secondary".to_owned(), Value::Bool(true));
            }
            Ok(root)
        })?;
        self.ret_h(Value::Node(root))
    }

    fn space_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let name = self.text(a.h(0)?)?;
        let size = Space::is_size(&name);
        if size != (imp == Imp::SpaceF) {
            return trap(format!(
                "`space.{name}` is read with {}",
                if size { "space_f" } else { "space_i" }
            ));
        }
        let v = self.with_view(|vs| {
            let v = vs
                .space
                .field(&name)
                .ok_or_else(|| format!("`space` has no field `{name}`"))?;
            vs.space_reads.push((name.clone(), v));
            Ok(v)
        })?;
        if size {
            ret_f64(v)
        } else {
            ret_i64(float_to_class(v))
        }
    }

    pub(crate) fn ui_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::UiNode => {
                let (kind, key) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                let at = self.with_view(|vs| {
                    vs.nodes.push(NodeBuild {
                        kind,
                        key,
                        props: BTreeMap::default(),
                        children: Vec::new(),
                        binding: None,
                        events: BTreeMap::default(),
                        anchor: None,
                    });
                    Ok(vs.nodes.len().saturating_sub(1))
                })?;
                self.ret_h(Value::Node(at))
            }
            Imp::UiPropH => {
                let v = self.val(a.h(2)?)?;
                self.set_prop(a.h(0)?, a.h(1)?, v)
            }
            Imp::UiPropI => self.set_prop(a.h(0)?, a.h(1)?, Value::Int(a.long(2)?)),
            Imp::UiPropB => self.set_prop(a.h(0)?, a.h(1)?, Value::Bool(a.h(2)? != 0)),
            Imp::UiPropF => self.set_prop(a.h(0)?, a.h(1)?, Value::Float(a.float(2)?)),
            Imp::UiPropPic => {
                let src = self.text(a.h(2)?)?;
                self.set_prop(a.h(0)?, a.h(1)?, Value::Picture(src))
            }
            Imp::UiChild => {
                let (parent, child) = (self.node_index(a.h(0)?)?, self.node_index(a.h(1)?)?);
                self.with_view(|vs| {
                    if vs.nodes.get(child).is_none() {
                        return Err("unknown ui node".to_owned());
                    }
                    let p = vs.nodes.get_mut(parent).ok_or("unknown ui node")?;
                    p.children.push(child);
                    Ok(())
                })?;
                ret_void()
            }
            Imp::UiBind => self.ui_bind(a),
            Imp::UiBindBegin => self.ui_bind_begin(a),
            Imp::UiOn => self.ui_on(a),
            Imp::UiChoose => self.ui_choose(a),
            Imp::UiActions => self.offer_actions(a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::UiAction => self.offer_action(a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::UiAnchor => {
                let node = self.node_index(a.h(0)?)?;
                let anchor = self.text(a.h(1)?)?;
                self.with_view(|vs| {
                    vs.nodes.get_mut(node).ok_or("unknown ui node")?.anchor = Some(anchor);
                    Ok(())
                })?;
                ret_void()
            }
            Imp::SpaceI | Imp::SpaceF => self.space_import(imp, a),
            Imp::RowOpen => ret_i32(i32::from(self.with_view(|vs| Ok(vs.open))?)),
            Imp::StateGet => self.state_get(a),
            Imp::StateShown => self.state_shown(a.h(0)?, a.h(1)?),
            Imp::StateAt => self.state_at(a),
            Imp::ArgAt => self.arg_at(a),
            Imp::UiBindState => self.bind_state(a, None),
            Imp::UiBindStateAt => {
                let at = usize::try_from(a.long(2)?)
                    .or_else(|_| trap("`bind:` takes a place in a list from 0"))?;
                self.bind_state(a, Some(at))
            }
            Imp::ActSet => self.act_set(a),
            Imp::ActKeep => self.act_keep(a),
            Imp::KeptGet => self.kept_get(a),
            Imp::ActThen => self.act_then(a),
            Imp::ActAnswer => self.act_answer(a),
            Imp::ActChoose => self.act_choose(a, CHOOSE_ACTION),
            Imp::ActChooseEditing => self.act_choose(a, CHOOSE_EDITING_ACTION),
            Imp::ActChooseFolder => self.act_choose(a, CHOOSE_FOLDER_ACTION),
            Imp::ActSave => self.act_save(a),
            Imp::ActBlur => self.act_blur(),
            Imp::ActClose => self.act_close(),
            Imp::ActMadeFrom => self.act_made_from(a),
            Imp::ActMany => self.act_many(a),
            Imp::ActOpen => self.act_open(a),
            Imp::ActCall => self.act_call(a),
            Imp::ActCallInput => self.act_call_input(a),
            Imp::ActCallVolume => self.act_call_volume(a),
            Imp::ActRunLine => self.act_run_line(a),
            Imp::ActUndo => self.act_undo(a),
            Imp::ChangeView => self.change_view(a),
            _ => trap("not a ui import"),
        }
    }

    /// A node bound to a field of a record: what it shows is the field, and what the person
    /// types into it writes the field.
    fn ui_bind(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let node = self.node_index(a.h(0)?)?;
        let r = self.ref_arg(a.h(1)?)?;
        let field = self.text(a.h(2)?)?;
        let (_, f) = self.lookup_field(&r.target, &field)?;
        let binding = Binding {
            target: BindTarget::Field {
                target: Rc::unwrap_or_clone(r.target),
                field: f.id,
            },
            author: self.module_of_frame()?,
        };
        self.with_view(|vs| {
            vs.nodes.get_mut(node).ok_or("unknown ui node")?.binding = Some(binding);
            Ok(())
        })?;
        ret_void()
    }

    /// A node that begins binding `field`, which the next node it holds takes.
    fn ui_bind_begin(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let node = self.node_index(a.h(0)?)?;
        let field = self.text(a.h(1)?)?;
        self.with_view(|vs| {
            vs.nodes
                .get_mut(node)
                .ok_or("unknown ui node")?
                .props
                .insert(crate::view::BEGIN_BIND.to_owned(), Value::Text(field));
            Ok(())
        })?;
        ret_void()
    }

    /// `on event: action` on a node: the action runs when the node's component fires `event`.
    fn ui_on(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let node = self.node_index(a.h(0)?)?;
        let event = self.text(a.h(1)?)?;
        let act = match self.val(a.h(2)?)? {
            v @ Value::Action(_) => v,
            other => {
                return trap(format!(
                    "expected an action reference, got {}",
                    other.kind_name()
                ));
            }
        };
        // A control bound to an action that cannot run now stands disabled, saying why: the
        // action's own `require`, or the availability the host publishes of its app's action
        // (a Rust app refuses the press with the same reason, and the person reads it before
        // pressing). A menu row carries its refusal in `disabled`, every other control in
        // `enabled`; both keep the reason in `description`, which the row and the control read
        // and a screen reader says, keys and all.
        let refused = match (&act, event.as_str()) {
            (Value::Action(run), "press" | "choose" | "activate" | "swipe") => {
                let viewer = self.with_view(|vs| Ok(vs.viewer.clone()))?;
                match self.unmet(run, &viewer)? {
                    Some(reason) => Some(reason),
                    None => self.refused_now(&run.action, &run.args),
                }
            }
            _ => None,
        };
        self.with_view(|vs| {
            let at = vs.nodes.get_mut(node).ok_or("unknown ui node")?;
            at.events.insert(event, act);
            if let Some(reason) = refused {
                // Only what the component declares: a window refuses a whole patch that hands a
                // component a prop it does not know, so a swipe or a row that has no way to stand
                // disabled keeps its look, and the press itself is refused with the reason.
                let declared = at
                    .kind
                    .strip_prefix("ui.")
                    .and_then(|name| tilolang::toolkit::catalogue().get(name));
                let known = |prop: &str| declared.is_some_and(|it| it.prop(prop).is_some());
                let menu = at.kind == "ui.MenuItem";
                let flag = if menu { "disabled" } else { "enabled" };
                if known(flag) {
                    at.props.insert(flag.to_owned(), Value::Bool(menu));
                }
                if known("description") {
                    at.props
                        .insert("description".to_owned(), Value::text(reason));
                }
            }
            Ok(())
        })?;
        ret_void()
    }

    /// `ui_choose`: the press of the case option `a(0)` takes the case `a(1)` into the field the
    /// option belongs to. A window shows the cases in a menu of its own and sends the pick; where
    /// the view runs without one (a test, an agent) the option is pressed directly.
    fn ui_choose(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let node = self.node_index(a.h(0)?)?;
        let case = self.text(a.h(1)?)?;
        self.with_view(|vs| {
            vs.nodes
                .get_mut(node)
                .ok_or("unknown ui node")?
                .events
                .insert(
                    "press".to_owned(),
                    Value::Action(ActionRef::new(CHOOSE_CASE_ACTION, vec![Value::Text(case)])),
                );
            Ok(())
        })?;
        ret_void()
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "class indexes and flags are small whole numbers"
)]
fn float_to_class(v: f64) -> i64 {
    v as i64
}

/// The event an action offered in the prop `prop` fires: `actions.2` for the third. The UI
/// host's validator takes the same form (`tilo_render_protocol::patch::validate::offered_event`).
fn offered_event(prop: &str, index: usize) -> String {
    format!("{prop}.{index}")
}

/// One `host.Action` (`label`, `icon`, `role`, `primary`, `destructive`, `on`, `key`,
/// `params`, `run`, `group`) as its control shows it, and the action reference its
/// event, `event`, runs.
fn offered_action(action: &Value, event: &str) -> HResult<(Value, Value)> {
    let Value::Tuple(fields) = action else {
        return trap("an offered action is a host.Action");
    };
    let text = |at: usize| match fields.get(at) {
        Some(Value::Text(text)) => Some(text.clone()),
        Some(Value::Opt(Some(held))) => held.as_text().map(str::to_owned),
        _ => None,
    };
    let flag = |at: usize| match fields.get(at) {
        Some(Value::Bool(flag)) => Some(*flag),
        Some(Value::Int(n)) => Some(*n != 0),
        Some(Value::Opt(Some(held))) => match **held {
            Value::Bool(flag) => Some(flag),
            Value::Int(n) => Some(n != 0),
            _ => None,
        },
        _ => None,
    };
    let Some(run @ Value::Action(_)) = fields.get(8).cloned() else {
        return trap("an offered action runs an action reference");
    };
    Ok((
        Value::Offered(Rc::new(tilolang::toolkit::Offered {
            label: text(0).unwrap_or_default(),
            icon: text(1),
            primary: flag(3).unwrap_or(false),
            destructive: flag(4).unwrap_or(false),
            on: flag(5),
            key: text(6),
            // An action its app publishes as unable to run now is offered disabled, with the
            // reason the action layer refuses it with.
            description: (!flag(10).unwrap_or(true)).then(|| text(11)).flatten(),
            event: event.to_owned(),
        })),
        run,
    ))
}

/// Put a node a change shows in the arena `vs` builds, its children first: the index of the
/// node. A node a change shows carries no events of its own.
fn push_tree(vs: &mut ViewState, node: UiNode) -> usize {
    let UiNode {
        kind,
        key,
        props,
        binding,
        events: _,
        anchor,
        children,
    } = node;
    let children = children
        .into_iter()
        .map(|child| push_tree(vs, child))
        .collect();
    vs.nodes.push(NodeBuild {
        kind,
        key,
        props,
        children,
        binding,
        events: BTreeMap::new(),
        anchor,
    });
    vs.nodes.len().saturating_sub(1)
}

/// A node showing something a change holds, keyed `key`.
fn shown_node_of(
    kind: &str,
    key: &str,
    props: Vec<(&str, Value)>,
    children: Vec<UiNode>,
) -> UiNode {
    UiNode {
        kind: kind.to_owned(),
        key: key.to_owned(),
        props: props
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
        binding: None,
        events: BTreeMap::default(),
        anchor: None,
        children,
    }
}

/// A text node saying `value`, quietly (in secondary ink) when `quiet`.
fn shown_text(key: &str, value: Value, quiet: bool) -> UiNode {
    let mut props = vec![("value", value)];
    if quiet {
        props.push(("secondary", Value::Bool(true)));
    }
    shown_node_of("ui.Text", key, props, Vec::new())
}

/// A chip naming `label`, with `icon` before it when there is one.
fn shown_chip(key: &str, label: Value, icon: Option<Value>) -> UiNode {
    let mut props = vec![("label", label)];
    if let Some(icon) = icon {
        props.push(("icon", icon));
    }
    shown_node_of("ui.Chip", key, props, Vec::new())
}

/// A day or a time of day, as the text component shows it (`value:as`); a day the change moved
/// with the time of day it holds reads as one fact, and carries it beside it (`value:time`).
fn shown_moment(key: &str, kind: &str, value: Value, extra: Option<Value>) -> UiNode {
    let mut props = vec![("value", value), ("value:as", Value::Text(kind.to_owned()))];
    if let (Some(Value::Text(at)), "date") = (extra, kind) {
        props.push(("value:time", Value::Text(at)));
    }
    shown_node_of("ui.Text", key, props, Vec::new())
}

/// The node showing `shown` (`Core::shown`): a day as a day and a moment as a moment in the
/// person's format, a person as their avatar and name, a case of an enum as its label and icon,
/// a resource as its title, a flag as yes or no, nothing as a quiet "None", and the elements of
/// a set side by side. Keyed `key`.
pub(crate) fn shown_node(key: &str, shown: &Value, phrase: &dyn Fn(&str) -> String) -> UiNode {
    let parts = match shown {
        Value::List(items) if !items.is_empty() => {
            let children = items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let (label, icon) = match item {
                        Value::Tuple(parts) => (
                            parts.get(1).cloned().unwrap_or(Value::Unit),
                            match parts.get(2) {
                                Some(Value::Opt(Some(icon))) => Some((**icon).clone()),
                                _ => None,
                            },
                        ),
                        other => (other.clone(), None),
                    };
                    shown_chip(&format!("{key}/{index}"), label, icon)
                })
                .collect();
            return shown_node_of("row", key, vec![("wrap", Value::Bool(true))], children);
        }
        Value::Tuple(parts) => parts,
        _ => {
            return shown_text(key, Value::Text(phrase("tilolang-none-value")), true);
        }
    };
    let kind = parts.first().and_then(Value::as_text).unwrap_or("text");
    let value = parts.get(1).cloned().unwrap_or(Value::Unit);
    let extra = match parts.get(2) {
        Some(Value::Opt(Some(extra))) => Some((**extra).clone()),
        _ => None,
    };
    match kind {
        "none" => shown_text(key, Value::Text(phrase("tilolang-none-value")), true),
        // A title left empty, as the field's placeholder reads: the record's untitled words,
        // dimmed.
        "placeholder" => shown_text(key, value, true),
        "date" | "time" => shown_moment(key, kind, value, extra),
        "actor" => {
            let avatar = shown_node_of(
                "ui.Avatar",
                &format!("{key}/who"),
                vec![("who", value)],
                Vec::new(),
            );
            let name = shown_text(&format!("{key}/name"), extra.unwrap_or(Value::Unit), false);
            shown_node_of("row", key, Vec::new(), vec![avatar, name])
        }
        "bool" => {
            let id = if matches!(value, Value::Bool(true)) {
                "tilolang-yes"
            } else {
                "tilolang-no"
            };
            shown_text(key, Value::Text(phrase(id)), false)
        }
        "case" => shown_chip(key, value, extra),
        // A rich text a change made: its paragraphs, drawn by the shared rich text
        // component, the runs it inserted marked and the runs it deleted struck.
        "rich" => shown_node_of("ui.BlockEditor", key, vec![("value", value)], Vec::new()),
        "text" if value.as_text().is_some_and(str::is_empty) => {
            shown_text(key, Value::Text(phrase("tilolang-empty-value")), true)
        }
        _ => shown_text(key, value, false),
    }
}
