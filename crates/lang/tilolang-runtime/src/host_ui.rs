//! Imports that build a view's UI tree. Views only.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_f64, ret_i64, ret_void, trap};
use crate::engine::Wasm;
use crate::handles::{FrameKind, ViewState};
use crate::space::Space;
use crate::value::{ActionRef, Value};
use crate::view::{BindTarget, Binding, NodeBuild};
use crate::view_state::{StateKey, StateLayer};

/// The action id of `set(name, value)`: changes a view state slot. Its arguments are the
/// window, the person (empty for window state), the declaration id and the value.
pub const SET_ACTION: &str = "$set";
/// The action id of `open(r, view: v, front: f)`: its arguments are the resource, the view id
/// (empty for the default view of its type) and whether a narrow window brings it in front.
pub const OPEN_ACTION: &str = "$open";

/// The action of `a.then(b)`: its arguments are `a` and `b`, run in turn.
pub const THEN_ACTION: &str = "$then";

impl Core {
    fn with_view<R>(&self, f: impl FnOnce(&mut ViewState) -> Result<R, String>) -> HResult<R> {
        let mut frames = self.frames.borrow_mut();
        match &mut frames.top_mut()?.kind {
            FrameKind::View(vs) => Ok(f(vs)?),
            _ => trap("ui nodes can only be built inside a view"),
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
        if matches!(v, Value::Node(_) | Value::Rec(_)) {
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
            // `host.Action`: label, icon, role, primary, destructive, on, params, run.
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
            let Some(run @ Value::Action(_)) = fields.get(7).cloned() else {
                return trap("an offered action runs an action reference");
            };
            let event = offered_event(&name, index);
            offered.push(Value::Offered(Rc::new(tilolang::toolkit::Offered {
                label: text(0).unwrap_or_default(),
                icon: text(1),
                primary: flag(3).unwrap_or(false),
                destructive: flag(4).unwrap_or(false),
                on: flag(5),
                event: event.clone(),
            })));
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

    /// The slot a `window` or `lens` declaration of the running view names.
    fn state_key(&self, layer: i32, id: i32) -> HResult<StateKey> {
        let layer = StateLayer::from_code(layer)
            .map_or_else(|| trap(format!("unknown view state layer {layer}")), Ok)?;
        let state = self.text(id)?;
        self.with_view(|vs| {
            Ok(StateKey {
                window: vs.window.clone(),
                principal: (layer == StateLayer::Lens).then(|| vs.viewer.0.clone()),
                state,
            })
        })
    }

    fn state_get(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let key = self.state_key(a.h(0)?, a.h(1)?)?;
        let fallback = self.val_whole(a.h(2)?)?;
        let (value, generation) = {
            let states = self.view_states.borrow();
            (
                states.get(&key).cloned().unwrap_or(fallback),
                states.generation(&key),
            )
        };
        let seen = value.clone();
        self.with_view(|vs| {
            vs.state_reads.push((key.clone(), generation));
            vs.state_seen.push((key, seen));
            Ok(())
        })?;
        self.ret_h(value)
    }

    /// The view state slot itself, as an argument of an action reference: it is read when the
    /// event fires ([`Value::State`]).
    fn state_at(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let key = self.state_key(a.h(0)?, a.h(1)?)?;
        let scalar = a.h(2)? != 0;
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

    fn bind_state(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let node = self.node_index(a.h(0)?)?;
        let key = self.state_key(a.h(1)?, a.h(2)?)?;
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
            target: BindTarget::State { key, name, ty },
            author: self.module_of_frame()?,
        };
        self.with_view(|vs| {
            vs.nodes.get_mut(node).ok_or("unknown ui node")?.binding = Some(binding);
            Ok(())
        })?;
        ret_void()
    }

    fn act_set(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let key = self.state_key(a.h(0)?, a.h(1)?)?;
        let value = self.val_whole(a.h(2)?)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action: SET_ACTION.to_owned(),
            args: vec![
                Value::Text(key.window),
                Value::Text(key.principal.unwrap_or_default()),
                Value::Text(key.state),
                value,
            ],
            author,
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
            action: THEN_ACTION.to_owned(),
            args: vec![first, next],
            author,
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
        }))
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
            Imp::UiPropF => self.set_prop(a.h(0)?, a.h(1)?, Value::Float(a.float(2)?)),
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
            Imp::UiBind => {
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
            Imp::UiOn => {
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
                self.with_view(|vs| {
                    vs.nodes
                        .get_mut(node)
                        .ok_or("unknown ui node")?
                        .events
                        .insert(event, act);
                    Ok(())
                })?;
                ret_void()
            }
            Imp::UiActions => self.offer_actions(a.h(0)?, a.h(1)?, a.h(2)?),
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
            Imp::StateGet => self.state_get(a),
            Imp::StateAt => self.state_at(a),
            Imp::ArgAt => self.arg_at(a),
            Imp::UiBindState => self.bind_state(a),
            Imp::ActSet => self.act_set(a),
            Imp::ActThen => self.act_then(a),
            Imp::ActOpen => self.act_open(a),
            _ => trap("not a ui import"),
        }
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
