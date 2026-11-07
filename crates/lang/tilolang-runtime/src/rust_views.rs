//! Rust apps' views, as the runtime draws them.
//!
//! A tilolang view is the runtime's own code: a package's module, its view export called. A Rust
//! app's view is a function in Rust, not in a package's module, and the runtime draws it the
//! same way a boundary — keyed, wired, diffed and read back — so the host builds the widgets of
//! both through the one catalogue (one-app-model.md §2: only who computes the tree differs).
//!
//! The host that runs a Rust app tells the runtime its views through a [`RustViews`]: a lookup
//! from an app, a resource's kind and a view name to the tree the view builds, which the runtime
//! calls where it calls a package's view export. The runtime holds the trait rather than the
//! crate, so it stays the same host for either language and the crate is the host's to link.
//!
//! What a Rust view's events do reaches the runtime as data, never a closure: each node's event
//! names an action on its resource, a place to open, or a state to set, and the runtime turns it
//! into the action reference a tilolang view's event would hold. A press therefore goes through
//! the same door, the same attribution and the same undo.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::ids::ModuleHash;
use crate::value::{ActionRef, ResourceRef, Value};
use crate::view::{Binding, UiNode};

/// A node of a Rust view, as the view built it: the same fields the wire's node has, so the host
/// draws it as it draws a tilolang view's node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RustNode {
    pub kind: String,
    pub key: String,
    pub props: BTreeMap<String, Value>,
    pub binding: Option<RustBinding>,
    pub anchor: Option<String>,
    pub children: Vec<RustNode>,
}

/// What a Rust view's binding edits: the field of a resource, or the view's own state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RustBinding {
    /// A field of the resource `target`, by its id string, and the field's id.
    Field { target: String, field: String },
    /// The view's own state `name`, held in the person's window as a tilolang view's `state`
    /// declaration is: so a control the view binds it to has somewhere to type into.
    State { name: String },
}

/// What pressing an event of a Rust view runs.
#[derive(Clone, Debug, PartialEq)]
pub enum RustEvent {
    /// The action `action` of `resource`, with these arguments (the resource itself is the
    /// subject, not an argument): the resource the view shows, or another it offered it on.
    Invoke {
        resource: String,
        action: String,
        args: Vec<Value>,
    },
    /// Show `target`, through `view` when it names one.
    Open {
        target: String,
        ty: String,
        view: Option<String>,
    },
    /// Show the resource the event carries (a list's `activate(x)`): the row that was pressed.
    OpenEvent { view: Option<String> },
    /// Set the view's own state `name` to the event's value.
    State { name: String },
}

/// What a view's function answers: the tree it built, and the events its nodes fire by the key
/// and event name it gave each.
pub struct RustRendered {
    pub tree: RustNode,
    pub events: BTreeMap<(String, String), RustEvent>,
}

/// A view a Rust app declares, to the runtime: its name and whether it is its type's default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustViewDecl {
    pub name: String,
    pub is_default: bool,
}

/// A Rust app's views, as the host that runs the app hands them to the runtime.
pub trait RustViews {
    /// The app whose views these are (`tube`).
    fn app(&self) -> &'static str;

    /// Each kind the app's views take, each with the kinds' names (`tube.Channel`): what the
    /// runtime finds a resource's views by.
    fn kinds(&self) -> Vec<String>;

    /// The views of `kind` (a type's kind, `tube.Channel`), each by name.
    fn views_of(&self, kind: &str) -> Vec<RustViewDecl>;

    /// The view `name` of a resource of `kind`, or the kind's default view when `name` is empty
    /// or names none.
    fn resolve(&self, kind: &str, name: &str) -> Option<RustViewDecl>;

    /// The tree the view `name` builds for `target`, whose reading is `reading` (its JSON, none
    /// before one arrived), for the person `person`, in the window `window`.
    fn render(
        &self,
        kind: &str,
        name: &str,
        target: &RustResource,
        reading: Option<&serde_json::Value>,
        person: &crate::ids::Principal,
        window: &str,
    ) -> RustRendered;
}

/// A resource a Rust view shows: its id and type kind, as the runtime names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustResource {
    /// Its id, as the runtime names it.
    pub id: String,
    /// Its type's kind (`tube.Channel`).
    pub kind: String,
}

/// One app's views, held by the runtime.
pub(crate) type Shared = Rc<std::cell::RefCell<BTreeMap<String, Rc<dyn RustViews>>>>;

/// The key a registry that answers for every app's views is held under: a process that links
/// several Rust apps' views at once (the session, a test), rather than one app per process.
pub const EVERY_APP: &str = "";

/// The app a resource of type `ty` (`tube.Channel`, or a foreign reading's `tube#Channel`) is
/// drawn by, from the registries the runtime was told: the app whose name `ty` starts with, else
/// the one registry that answers for every app.
pub(crate) fn app_of(shared: &Shared, ty: &str) -> Option<Rc<dyn RustViews>> {
    let app = ty.split(['#', '.']).next().unwrap_or(ty);
    let held = shared.borrow();
    held.get(app).or_else(|| held.get(EVERY_APP)).map(Rc::clone)
}

/// The kind a Rust app declares for the resource type `ty`: its own kind whose type name is
/// `ty`'s (`tube.Channel` for `tube#Channel`), when it declares one.
///
/// The app matters as well as the type's name: two apps declare a `Channel` (`tube.Channel`,
/// `chat.Channel`), and a resource of one is drawn by its own app's view, never the other's.
pub(crate) fn kind_of(views: &Rc<dyn RustViews>, ty: &str) -> String {
    let app = ty.split(['#', '.']).next().unwrap_or(ty);
    let name = ty.rsplit(['#', '.']).next().unwrap_or(ty);
    views
        .kinds()
        .into_iter()
        .find(|kind| {
            kind.split(['#', '.']).next() == Some(app)
                && kind.rsplit(['#', '.']).next() == Some(name)
        })
        .or_else(|| {
            // A kind a registry of every app names by the type alone.
            views
                .kinds()
                .into_iter()
                .find(|kind| kind.rsplit(['#', '.']).next() == Some(name))
        })
        .unwrap_or_else(|| format!("{app}.{name}"))
}

/// The view a place's `shown_as` names the resource type `ty` through, or the type's default view:
/// what the runtime draws a resource of a Rust app's type with.
pub(crate) fn view_for(views: &Rc<dyn RustViews>, ty: &str, name: &str) -> Option<RustViewDecl> {
    let kind = kind_of(views, ty);
    views.resolve(&kind, name)
}

impl RustNode {
    /// This node as the runtime's, its events kept as the action references a tilolang view's
    /// events hold, and its binding as the field edit the runtime applies.
    ///
    /// # Errors
    ///
    /// An event that names no resource the runtime can parse.
    pub(crate) fn into_ui(
        self,
        events: &BTreeMap<(String, String), RustEvent>,
        subject: &ResourceRef,
        author: &ModuleHash,
        window: &str,
        viewer: &crate::ids::Principal,
        retain: &mut dyn FnMut(Value) -> crate::handles::Retained,
    ) -> Result<UiNode, String> {
        let key = self.key.clone();
        let mut kept: BTreeMap<String, crate::handles::Retained> = BTreeMap::new();
        for ((node_key, name), effect) in events {
            if node_key != &key {
                continue;
            }
            let aref = action(effect, subject, window, viewer, author)?;
            kept.insert(name.clone(), retain(Value::Action(aref)));
        }
        let binding = match &self.binding {
            Some(RustBinding::Field { target, field }) => {
                let at = ResourceRef::parse_id(target, String::new())
                    .ok_or_else(|| format!("no resource `{target}` to bind"))?;
                Some(Binding {
                    target: crate::view::BindTarget::Field {
                        target: at,
                        field: field.clone(),
                    },
                    author: author.clone(),
                })
            }
            // The view's own state, in the person's window: where a bound control types into.
            Some(RustBinding::State { name }) => Some(Binding {
                target: crate::view::BindTarget::State {
                    key: crate::view_state::StateKey::view(window, viewer.0.clone(), name.clone()),
                    name: name.clone(),
                    ty: "Text".to_owned(),
                },
                author: author.clone(),
            }),
            None => None,
        };
        let mut children = Vec::with_capacity(self.children.len());
        for child in self.children {
            children.push(child.into_ui(events, subject, author, window, viewer, retain)?);
        }
        Ok(UiNode {
            kind: self.kind,
            key,
            props: self.props,
            binding,
            events: kept,
            anchor: self.anchor,
            children,
        })
    }
}

/// The action reference a Rust view's event names, as the runtime's. An action on the view's own
/// resource is the empty resource in [`RustEvent::Invoke`], read here as `subject`.
fn action(
    effect: &RustEvent,
    subject: &ResourceRef,
    window: &str,
    viewer: &crate::ids::Principal,
    author: &ModuleHash,
) -> Result<ActionRef, String> {
    let author = Some(author.clone());
    Ok(match effect {
        RustEvent::Invoke {
            resource,
            action,
            args,
        } => {
            let at = if resource.is_empty() {
                subject.clone()
            } else {
                ResourceRef::parse_id(resource, String::new())
                    .ok_or_else(|| format!("no resource `{resource}` to invoke on"))?
            };
            let mut all = vec![Value::resource(&at)];
            all.extend(args.iter().cloned());
            ActionRef {
                action: action.clone(),
                args: all,
                author,
                from: None,
            }
        }
        RustEvent::Open { target, ty, view } => ActionRef {
            action: crate::host_ui::OPEN_ACTION.to_owned(),
            args: vec![
                ResourceRef::parse_id(target, ty.clone())
                    .map_or(Value::Unit, |at| Value::resource(&at)),
                view.clone().map_or_else(Value::none, Value::text),
                Value::Bool(true),
            ],
            author,
            from: None,
        },
        RustEvent::OpenEvent { view } => ActionRef {
            action: crate::host_ui::OPEN_ACTION.to_owned(),
            args: vec![
                Value::Event,
                view.clone().map_or_else(Value::none, Value::text),
                Value::Bool(true),
            ],
            author,
            from: None,
        },
        RustEvent::State { name } => ActionRef {
            action: crate::host_ui::SET_ACTION.to_owned(),
            args: vec![
                Value::Text(window.to_owned()),
                Value::text(viewer.0.clone()),
                Value::Text(name.clone()),
                Value::Event,
            ],
            author,
            from: None,
        },
    })
}
