//! What a test's host answers the calls a view makes to another app: it records each call,
//! leaves one the system asks the person about waiting for their yes, and answers every other
//! with unit. The press helpers fire a control as a person does, and a press whose action leaves
//! the system answers the confirm the system asks first: the call runs as the dispatch delivers
//! the person's yes.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{ActionRef, Actor, InvokeOutcome, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

/// What a host was asked, in order, and the answers it gives.
#[derive(Clone, Default)]
pub struct Sent(pub Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        // A call the system asks the person about first waits for their yes, which the test
        // answers as the dispatch delivers it. Answering it here would let the call look run
        // when the person was never asked.
        if matches!(invocation, Invocation::Action { asks: true, .. }) {
            return Invoked::Pending(format!("asked-{}", self.0.borrow().len()));
        }
        Invoked::Done(Value::Unit)
    }
}

impl Sent {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call sent so far, in order, left recorded.
    pub fn all(&self) -> Vec<Invocation> {
        self.0.borrow().clone()
    }

    /// Every call sent so far, in order, leaving the record empty.
    pub fn take(&self) -> Vec<Invocation> {
        std::mem::take(&mut *self.0.borrow_mut())
    }

    /// Every call sent so far as its name and arguments, leaving the record empty.
    pub fn take_calls(&self) -> Vec<(String, Vec<(String, Value)>)> {
        self.take()
            .into_iter()
            .filter_map(|invocation| match invocation {
                Invocation::Action { name, args, .. } => Some((name, args)),
                Invocation::Edit { .. } => None,
            })
            .collect()
    }

    /// The calls sent so far, as the `(name, arguments)` of each action.
    pub fn calls(&self) -> Vec<(String, Vec<(String, Value)>)> {
        self.all()
            .into_iter()
            .filter_map(|invocation| match invocation {
                Invocation::Action { name, args, .. } => Some((name, args)),
                Invocation::Edit { .. } => None,
            })
            .collect()
    }

    /// The last call sent.
    pub fn last(&self) -> Invocation {
        self.0
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("the app sends a verb"))
    }

    /// The app and action of the last call.
    pub fn called(&self) -> (String, String) {
        let Invocation::Action { app, name, .. } = self.last() else {
            panic!("the app sends an action: {:?}", self.last());
        };
        (app.to_string(), name)
    }

    /// The argument `named` of the last call.
    pub fn argument(&self, named: &str) -> Value {
        let Invocation::Action { args, .. } = self.last() else {
            panic!("the app sends an action: {:?}", self.last());
        };
        args.iter().find(|(name, _)| name == named).map_or_else(
            || panic!("the verb takes `{named}`: {args:?}"),
            |(_, value)| value.clone(),
        )
    }

    /// The arguments of the first `name` asked, by name.
    pub fn asked_args(&self, name: &str) -> Vec<(String, Value)> {
        self.0
            .borrow()
            .iter()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked, args, ..
                } if asked == name => Some(args.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks `{name}`: {:?}", self.0.borrow()))
    }

    /// The arguments of the first `name` asked, after the resource.
    pub fn asked_values(&self, name: &str) -> Vec<Value> {
        self.asked_args(name)
            .into_iter()
            .map(|(_, value)| value)
            .collect()
    }

    /// The arguments of the last `name` asked, by name.
    pub fn last_args(&self, name: &str) -> Vec<(String, Value)> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked, args, ..
                } if asked == name => Some(args.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks `{name}`: {:?}", self.0.borrow()))
    }

    /// The value of `wanted` in the last `name` asked.
    pub fn last_argument(&self, name: &str, wanted: &str) -> Value {
        self.last_args(name)
            .into_iter()
            .find(|(named, _)| named == wanted)
            .unwrap_or_else(|| panic!("{name} is given `{wanted}`"))
            .1
    }

    /// The arguments of the last `name` asked on `target`, by name: the call names the resource
    /// it acts on, and it is `target`.
    pub fn last_args_acting_on(
        &self,
        name: &str,
        target: &tilolang_runtime::ResourceRef,
    ) -> Vec<(String, Value)> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked,
                    args,
                    target: acted,
                    ..
                } if asked == name => {
                    let acted = acted
                        .as_ref()
                        .unwrap_or_else(|| panic!("{name} names the resource it acts on"));
                    assert_eq!(
                        acted.id_string(),
                        target.id_string(),
                        "{name} acts on the resource"
                    );
                    Some(args.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks `{name}`: {:?}", self.0.borrow()))
    }

    /// The value of `wanted` in the last `name` asked on `target`.
    pub fn last_argument_acting_on(
        &self,
        name: &str,
        wanted: &str,
        target: &tilolang_runtime::ResourceRef,
    ) -> Value {
        self.last_args_acting_on(name, target)
            .into_iter()
            .find(|(named, _)| named == wanted)
            .unwrap_or_else(|| panic!("{name} is given `{wanted}`"))
            .1
    }

    /// The arguments of the last `app`'s `name` asked, by name.
    pub fn last_args_on(&self, app: &str, name: &str) -> Vec<(String, Value)> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    app: asked_app,
                    name: asked,
                    args,
                    ..
                } if asked == name && asked_app.as_str() == app => Some(args.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks {app}'s `{name}`: {:?}", self.0.borrow()))
    }

    /// The arguments of the last `name` asked, by name, as the target's action API takes them:
    /// the resource it is asked on and the options it leaves out are not among them. `app` is
    /// the app the call must belong to.
    pub fn api_arguments(
        &self,
        app: &str,
        name: &str,
    ) -> serde_json::Map<String, serde_json::Value> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    app: asked_app,
                    name: asked,
                    ..
                } if asked == name => {
                    assert_eq!(asked_app.as_str(), app, "the app's own verb");
                    Some(invocation.api_arguments())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks `{name}`: {:?}", self.0.borrow()))
    }

    /// The names of the actions asked, in order.
    pub fn names(&self) -> Vec<String> {
        self.calls().into_iter().map(|(name, _)| name).collect()
    }

    /// The calls of `action`, each by its name and arguments.
    pub fn matching(&self, action: &str) -> Vec<(String, Vec<(String, Value)>)> {
        self.calls()
            .into_iter()
            .filter(|(name, _)| name == action || name == &action.replace('_', "-"))
            .collect()
    }

    /// The arguments (after the resource) of the first `action` asked.
    pub fn asked(&self, action: &str) -> Vec<Value> {
        let (_, args) = self
            .matching(action)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the app asks `{action}`; it asked {:?}", self.names()));
        args.into_iter().map(|(_, value)| value).collect()
    }

    /// The arguments (after the resource) of the last `action` asked.
    pub fn last_asked(&self, action: &str) -> Vec<Value> {
        let (_, args) = self
            .matching(action)
            .pop()
            .unwrap_or_else(|| panic!("the app asks `{action}`; it asked {:?}", self.names()));
        args.into_iter().map(|(_, value)| value).collect()
    }

    /// One argument of the first `action` asked, by its parameter name.
    pub fn arg(&self, action: &str, name: &str) -> Option<Value> {
        let (_, args) = self.matching(action).into_iter().next()?;
        args.into_iter()
            .find(|(arg, _)| arg == name)
            .map(|(_, value)| value)
    }
}

fn nodes_of<'a>(node: &'a UiNode, kind: &str, into: &mut Vec<&'a UiNode>) {
    into.extend(node.of_kind(&format!("ui.{kind}")));
}

/// `text` as a person reads it: without the marks that keep a name's direction apart from the
/// sentence around it.
fn plain(text: &str) -> String {
    text.replace(['\u{2068}', '\u{2069}'], "")
}

/// The control of `kind` labelled `label` in `window`: its key and the event a person's press of
/// it fires.
pub fn control(window: &ViewInstance, kind: &str, label: &str) -> (String, &'static str) {
    let mut found = Vec::new();
    nodes_of(window.tree(), kind, &mut found);
    let labelled = |node: &&&UiNode| {
        ["label", "title"].iter().any(
            |prop| matches!(node.props.get(*prop), Some(Value::Text(text)) if plain(text) == label),
        )
    };
    // The control the page shows, before the same action's row in the window's "…".
    let in_menu = |node: &&&UiNode| node.props.get("in_menu") == Some(&Value::Bool(true));
    let node = found
        .iter()
        .filter(labelled)
        .find(|node| !in_menu(node))
        .or_else(|| found.iter().find(labelled))
        .unwrap_or_else(|| {
            panic!(
                "a {kind} {label:?}:\n{}",
                crate::fixtures::drawn(window.tree())
            )
        });
    let event = match kind {
        "Button" => "press",
        "NavItem" => "select",
        _ => "activate",
    };
    (node.key.clone(), event)
}

/// Press the control of `kind` labelled `label` (a button's press, a row's or a place's
/// activation), as a person does.
pub fn press(window: &ViewInstance, kind: &str, label: &str) {
    let (key, event) = control(window, kind, label);
    let outcome = window.fire(&key, event).expect("the control acts");
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "{label}: {outcome:?}"
    );
}

/// Press the control of `kind` labelled `label` where the system confirms it first, because what
/// it does leaves the system, and answer the system's confirm as the person does: the asked call
/// runs as the dispatch delivers their yes. What the action answered.
pub fn try_press_confirmed(
    host: &Host,
    window: &ViewInstance,
    person: &Actor,
    kind: &str,
    label: &str,
) -> Result<InvokeOutcome, String> {
    let (key, event) = control(window, kind, label);
    try_press_confirmed_key(host, window, person, &key, event, label)
}

/// [`try_press_confirmed`] of a control named by its `key` and the `event` its press fires.
pub fn try_press_confirmed_key(
    host: &Host,
    window: &ViewInstance,
    person: &Actor,
    key: &str,
    event: &str,
    label: &str,
) -> Result<InvokeOutcome, String> {
    let outcome = window
        .fire(key, event)
        .map_err(|why| format!("{label}: {why}"))?;
    match outcome {
        InvokeOutcome::Asked(_) | InvokeOutcome::Waits { .. } => {
            answer_confirm(host, window, person, key, event, label, outcome)
        }
        other => Err(format!(
            "{label} waits for the person's yes first: {other:?}"
        )),
    }
}

/// The confirm `outcome` asked, answered as the person does, and each call the answer reaches
/// that asks too, until the action ends.
fn answer_confirm(
    host: &Host,
    window: &ViewInstance,
    person: &Actor,
    key: &str,
    event: &str,
    label: &str,
    outcome: InvokeOutcome,
) -> Result<InvokeOutcome, String> {
    let mut outcome = outcome;
    for _ in 0..64 {
        match outcome {
            // The pressed action itself leaves the system: the dispatch asked them about it, and
            // it runs on their yes.
            InvokeOutcome::Asked(asked) => {
                if asked.is_empty() {
                    return Err(format!("{label} names no call the person is asked about"));
                }
                let action = window
                    .event_action(key, event)
                    .ok_or_else(|| format!("{label} names the action it presses"))?;
                outcome = host
                    .runtime()
                    .invoke_approved_through(person, None, action, None)
                    .map_err(|why| why.to_string())?;
            }
            // The app's own action reached a call that leaves the system: the run waits on it,
            // and the person's yes answers the dispatch and the action runs on from there.
            InvokeOutcome::Waits { run, .. } => {
                outcome = host
                    .runtime()
                    .answer_call(&run, Invoked::Done(Value::Unit))
                    .map_err(|why| why.to_string())?
                    .into();
            }
            done => return Ok(done),
        }
    }
    Err(format!("{label} kept asking after 64 yeses"))
}

/// A view fired as a person fires it: where the system asks them first because what the control
/// does leaves the system, their yes answers it. What it came to, which never waits on a call.
pub trait Answered {
    /// Fire the control `key`'s `event`, answering a confirm the system asks first.
    ///
    /// # Errors
    ///
    /// The control's own failure, or the confirm's.
    fn fire_answering(
        &self,
        host: &Host,
        person: &Actor,
        key: &str,
        event: &str,
    ) -> Result<InvokeOutcome, String>;

    /// [`Self::fire_answering`] of a control whose event carries `value`.
    ///
    /// # Errors
    ///
    /// The control's own failure, or the confirm's.
    fn fire_with_answering(
        &self,
        host: &Host,
        person: &Actor,
        key: &str,
        event: &str,
        value: Option<&Value>,
    ) -> Result<InvokeOutcome, String>;

    /// [`Self::fire_answering`] of the control `key`'s press.
    ///
    /// # Errors
    ///
    /// The control's own failure, or the confirm's.
    fn press_answering(
        &self,
        host: &Host,
        person: &Actor,
        key: &str,
    ) -> Result<InvokeOutcome, String>;
}

impl Answered for ViewInstance {
    fn fire_answering(
        &self,
        host: &Host,
        person: &Actor,
        key: &str,
        event: &str,
    ) -> Result<InvokeOutcome, String> {
        let outcome = self.fire(key, event).map_err(|why| why.to_string())?;
        match outcome {
            InvokeOutcome::Asked(_) | InvokeOutcome::Waits { .. } => {
                answer_confirm(host, self, person, key, event, key, outcome)
            }
            other => Ok(other),
        }
    }

    fn fire_with_answering(
        &self,
        host: &Host,
        person: &Actor,
        key: &str,
        event: &str,
        value: Option<&Value>,
    ) -> Result<InvokeOutcome, String> {
        let outcome = self
            .fire_with(key, event, value)
            .map_err(|why| why.to_string())?;
        match outcome {
            InvokeOutcome::Asked(_) | InvokeOutcome::Waits { .. } => {
                answer_confirm(host, self, person, key, event, key, outcome)
            }
            other => Ok(other),
        }
    }

    fn press_answering(
        &self,
        host: &Host,
        person: &Actor,
        key: &str,
    ) -> Result<InvokeOutcome, String> {
        self.fire_answering(host, person, key, "press")
    }
}

/// [`try_press_confirmed`] of a press the action takes: the person pressed and said yes.
pub fn press_confirmed(
    host: &Host,
    window: &ViewInstance,
    person: &Actor,
    kind: &str,
    label: &str,
) {
    let outcome = try_press_confirmed(host, window, person, kind, label)
        .unwrap_or_else(|why| panic!("{label}: {why}"));
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "{label}: {outcome:?}"
    );
}

/// [`press_confirmed`] of a control named by its `key` and the `event` its press fires.
pub fn press_key_confirmed(
    host: &Host,
    window: &ViewInstance,
    person: &Actor,
    key: &str,
    event: &str,
    label: &str,
) {
    let outcome = try_press_confirmed_key(host, window, person, key, event, label)
        .unwrap_or_else(|why| panic!("{label}: {why}"));
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "{label}: {outcome:?}"
    );
}

/// Invoke `action` as `person` does and answer the confirm the system asks first, whether the
/// action itself leaves the system or a call its own code reaches does. What it came to.
pub fn invoke_confirmed(
    host: &Host,
    person: &Actor,
    action: impl Into<ActionRef>,
) -> Result<InvokeOutcome, String> {
    let action = action.into();
    let mut outcome = host
        .runtime()
        .invoke_action(person, action.clone())
        .map_err(|why| why.to_string())?;
    for _ in 0..64 {
        match outcome {
            InvokeOutcome::Asked(_) => {
                outcome = host
                    .runtime()
                    .invoke_approved_through(person, None, action.clone(), None)
                    .map_err(|why| why.to_string())?;
            }
            InvokeOutcome::Waits { run, .. } => {
                outcome = host
                    .runtime()
                    .answer_call(&run, Invoked::Done(Value::Unit))
                    .map_err(|why| why.to_string())?
                    .into();
            }
            done => return Ok(done),
        }
    }
    Err(format!("{} kept asking after 64 yeses", action.action))
}
