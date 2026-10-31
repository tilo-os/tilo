//! A part of a tilolang shell (its top bar first) shown on the desktop.
//!
//! A shell package's views run in a tilolang instance (`tilolang_tilo::instance`), exactly as
//! an app's do, and draw through the same UI host half (`tilo_ui_host::AppWindow`, here as a
//! [`AppWindow::part`]: the view as it is, no window shell). The toolkit lays that widget tree
//! out and the CPU rasterizes it, as [`WidgetHost`] does for every widget tree the session
//! shows; the compositor gets the buffer as a layer. Nothing here renders on its own.
//!
//! What the views read of the session comes in as resources whose origin is the session
//! (`type Desk (origin: session.desk)`): [`SessionOrigin`] writes what the session publishes on
//! its door (the `session:desk` resource's reading) into them, and carries the shell's outside
//! effects (`outside("session.act", ...)`) back to the door as invocations. A shell view never
//! reaches the session another way.
//!
//! The part works only when something changed: a press, a reading of the session's that moved,
//! or a door answer. Otherwise a frame asks nothing of the instance, and the toolkit draws
//! nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_compositor::{CursorIcon, HostKey, HostLayer, SurfaceHost};
use tilo_render_protocol::patch::local::pair;
use tilo_ui_host::AppWindow;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::{Delivery, NameHost, Principal, ResourceRef, Value};
use tilolang_tilo::tilolang::Compiled;
use ui::tilo_actions::Action as _;

use crate::{Placement, WidgetHost};

/// The origin the session's resources come from: the first part of their kinds.
pub const ORIGIN: &str = "session";
/// The resource the session publishes its desk as, whose reading [`SessionOrigin`] writes.
pub const DESK_RESOURCE: &str = "session:desk";
/// Its action that brings a workspace to the front, by key.
pub const SWITCH_WORKSPACE: &str = "switch-workspace";
/// The argument of [`SWITCH_WORKSPACE`] naming the workspace.
pub const WORKSPACE_ARG: &str = "workspace";
/// The resource whose argument-less actions `session.act` names.
pub const CHROME_RESOURCE: &str = "desktop:chrome";

/// The capability that switches workspace: its payload is the workspace's key.
const CAP_SWITCH: &str = "session.switch_workspace";
/// The capability that runs one of the desktop's own argument-less actions, by id.
const CAP_ACT: &str = "session.act";

/// How often the instance and its window may answer each other in one frame before the frame
/// is drawn as it is: a press, the action it ran, the change it committed, the effect settled.
const EXCHANGES: usize = 8;

/// A workspace as the desk's reading names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReading {
    pub key: String,
    pub name: String,
    pub current: bool,
}

/// A task running for the person, as the desk's reading names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReading {
    pub key: String,
    pub title: String,
    pub state: String,
}

/// Someone else here, as the desk's reading names them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonReading {
    pub key: String,
    pub name: String,
}

/// The reading of [`DESK_RESOURCE`]: the fields of the `session.desk` kind
/// (`tilolang::origin::KINDS`), as JSON on the door.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeskReading {
    pub person: String,
    pub clock: String,
    pub weekday: String,
    pub date: String,
    pub unread: u32,
    pub quiet: bool,
    pub status_panel: bool,
    pub workspace: String,
    pub strip: String,
    pub workspaces: Vec<WorkspaceReading>,
    pub tasks: Vec<TaskReading>,
    /// Each app's unread count as its badge shows it, by app id: read on the door (a dock, an
    /// agent); no `session.desk` field holds it yet.
    #[serde(default)]
    pub badges: BTreeMap<String, u32>,
    /// The others here in the workspace in front: its collaborators.
    #[serde(default)]
    pub people: Vec<PersonReading>,
    /// Whether the pointer is at, or resting against, the top edge of the output: a shell's bar
    /// that hides until it is reached for shows while this holds.
    #[serde(default)]
    pub bar_reached: bool,
    /// Whether a window stands where a top bar rides: a bar that hides while a window overlaps
    /// it hides while this holds.
    #[serde(default)]
    pub bar_covered: bool,
    /// Whether the pointer is at the bottom edge of the output: a dock there shows while this
    /// holds.
    #[serde(default)]
    pub dock_reached: bool,
    /// Whether a window stands where a dock rides.
    #[serde(default)]
    pub dock_covered: bool,
}

/// What a shell asked of the session: an invocation on the door.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ask {
    pub resource: String,
    pub action: String,
    pub arguments: BTreeMap<String, String>,
}

/// How far from an output's edge the pointer resting there counts as at it: a bar or dock
/// pinned to a hiding edge is reached for by coming within this of that edge. The reach pad
/// the desk's own widgets use, so a glove and a mouse reach the same distance.
#[must_use]
pub fn reach_pad() -> f64 {
    f64::from(ui::tokens::Spacing::BASE.s2)
}

/// A window as the compositor shows it, for what the chrome needs of it: where it stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl WindowRect {
    fn covers(&self, edge_top: bool, band: f64, outputs: (i32, i32, u32, u32)) -> bool {
        let (x0, _, width, height) = outputs;
        let (left, right) = (f64::from(x0), f64::from(x0) + f64::from(width));
        let horizontal = self.x < right && self.x + self.width > left;
        if !horizontal {
            return false;
        }
        if edge_top {
            // A window reaching into the top band covers a bar there.
            let top = f64::from(outputs.1);
            self.y < top + band && self.y + self.height > top
        } else {
            // A window reaching into the bottom band covers a dock there.
            let bottom = f64::from(outputs.1) + f64::from(height);
            self.y < bottom && self.y + self.height > bottom - band
        }
    }
}

/// What the screen says of the edges a bar or dock that shows only sometimes rides: whether
/// the pointer is at each edge, and whether a window stands where a bar or dock there would.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenEdges {
    pub bar_reached: bool,
    pub bar_covered: bool,
    pub dock_reached: bool,
    pub dock_covered: bool,
}

/// Which edges the shell's chrome stands on: what the part covers of the outputs. The session
/// reads it from the shell's record — a bar on the top or bottom, a dock on the bottom or the
/// top — and the part covers each band it names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostEdges {
    /// The bar rides an edge (whether it shows now or not): the part covers its band.
    pub bar: bool,
    /// The bar rides the top rather than the bottom.
    pub bar_top: bool,
    /// The dock rides an edge.
    pub dock: bool,
    /// The dock rides the bottom rather than the top.
    pub dock_bottom: bool,
}

impl HostEdges {
    /// Whether the bar's band is at the top.
    #[must_use]
    pub const fn bar_at_top(&self) -> bool {
        self.bar && self.bar_top
    }

    /// Whether the dock's band is at the bottom.
    #[must_use]
    pub const fn dock_at_bottom(&self) -> bool {
        self.dock && self.dock_bottom
    }
}

/// What a screen says of its edges: the pointer at `pointer` and the windows `windows`, over
/// `outputs` (their bounding box, `(x, y, w, h)`), for a bar and a dock of `band` logical
/// pixels. The pointer is at an edge when it rests within [`reach_pad`] of the top or bottom
/// output edge there, or over the band a shown bar or dock would take.
#[must_use]
pub fn screen_edges(
    outputs: (i32, i32, u32, u32),
    band: f64,
    pointer: (f64, f64),
    windows: &[WindowRect],
) -> ScreenEdges {
    let (x0, y0, width, height) = outputs;
    let (x, y) = pointer;
    let (left, right) = (f64::from(x0), f64::from(x0) + f64::from(width));
    let (top, bottom) = (f64::from(y0), f64::from(y0) + f64::from(height));
    let over = x >= left && x < right;
    let pad = reach_pad();
    let at_top = over && y >= top - pad && y <= top + pad;
    let at_bottom = over && y <= bottom + pad && y >= bottom - pad;
    ScreenEdges {
        // A bar reached for is reached at its own edge, or by the pointer coming to rest on
        // the band it shows in.
        bar_reached: at_top || (over && y >= top - pad && y <= top + band),
        dock_reached: at_bottom || (over && y <= bottom + pad && y >= bottom - band),
        bar_covered: windows.iter().any(|w| w.covers(true, band, outputs)),
        dock_covered: windows.iter().any(|w| w.covers(false, band, outputs)),
    }
}

/// Between the session and the shell's origin: the desk's reading the session last published,
/// what the shell asked of the door, and the door's answers. The session serves it every turn
/// of its loop ([`SessionLink::serve`]); the origin reads and writes the rest.
#[derive(Default)]
pub struct SessionLink {
    reading: RefCell<Option<DeskReading>>,
    /// A reading arrived the origin has not written yet.
    fresh: Cell<bool>,
    asked: RefCell<VecDeque<Ask>>,
    /// How many effects the shell handed the session, and how many the session ran.
    handed: Cell<u64>,
    served: Cell<u64>,
    answers: RefCell<BTreeMap<Ask, Result<(), String>>>,
}

impl SessionLink {
    /// The session published `reading`: the shell writes it on its next frame when it differs
    /// from the last. Whether it did.
    pub fn publish(&self, reading: DeskReading) -> bool {
        // What the desk reading names is the desk; what the screen says of its edges comes
        // with it, published by [`Self::publish_edges`], so the reading keeps the last word on
        // it: a desk change does not put the edges back to rest.
        let mut reading = reading;
        if let Some(was) = self.reading.borrow().as_ref() {
            reading.bar_reached = was.bar_reached;
            reading.bar_covered = was.bar_covered;
            reading.dock_reached = was.dock_reached;
            reading.dock_covered = was.dock_covered;
        }
        if self.reading.borrow().as_ref() == Some(&reading) {
            return false;
        }
        *self.reading.borrow_mut() = Some(reading);
        self.fresh.set(true);
        true
    }

    /// Run what the shell asked through `invoke` (the session's door), keeping each answer
    /// for the effect that asked. Whether anything ran.
    pub fn serve(&self, mut invoke: impl FnMut(&Ask) -> Result<(), String>) -> bool {
        let asked: Vec<Ask> = self.asked.borrow_mut().drain(..).collect();
        self.served
            .set(self.served.get().wrapping_add(asked.len() as u64));
        for ask in &asked {
            let answer = invoke(ask);
            if let Err(why) = &answer {
                tracing::warn!(?ask, why, "the session refused what the shell asked");
            }
            self.answers.borrow_mut().insert(ask.clone(), answer);
        }
        !asked.is_empty()
    }

    /// Whether the shell asked something the session has not run yet.
    #[must_use]
    pub fn has_asks(&self) -> bool {
        !self.asked.borrow().is_empty()
    }

    /// Publish what the screen says of its edges (`edges`), folded into the desk reading the
    /// session publishes: the pointer at an edge and a window standing where a bar or a dock
    /// there would. Published before the desk so a frame that moves both writes them once.
    pub fn publish_edges(&self, edges: ScreenEdges) {
        let Some(mut reading) = self.reading.borrow().clone() else {
            return;
        };
        if reading.bar_reached == edges.bar_reached
            && reading.bar_covered == edges.bar_covered
            && reading.dock_reached == edges.dock_reached
            && reading.dock_covered == edges.dock_covered
        {
            return;
        }
        reading.bar_reached = edges.bar_reached;
        reading.bar_covered = edges.bar_covered;
        reading.dock_reached = edges.dock_reached;
        reading.dock_covered = edges.dock_covered;
        let _ = self.publish(reading);
    }
}

/// The ask an outside effect of `capability` with `payload` is, or why it is none.
fn ask_of(capability: &str, payload: &Json) -> Result<Ask, String> {
    let Json::String(word) = payload else {
        return Err(format!("{capability} takes a text, not {payload}"));
    };
    match capability {
        CAP_SWITCH => Ok(Ask {
            resource: DESK_RESOURCE.to_owned(),
            action: SWITCH_WORKSPACE.to_owned(),
            arguments: BTreeMap::from([(WORKSPACE_ARG.to_owned(), word.clone())]),
        }),
        CAP_ACT => Ok(Ask {
            resource: CHROME_RESOURCE.to_owned(),
            action: word.clone(),
            arguments: BTreeMap::new(),
        }),
        other => Err(format!("the session takes no outside effect `{other}`")),
    }
}

/// What the origin last wrote of an item kept by key (a workspace, a task).
struct Kept<T> {
    at: ResourceRef,
    was: T,
}

/// The session as the origin of a shell's `session.*` resources. See the module docs.
pub struct SessionOrigin {
    link: Rc<SessionLink>,
    written: Option<DeskReading>,
    workspaces: BTreeMap<String, Kept<WorkspaceReading>>,
    tasks: BTreeMap<String, Kept<TaskReading>>,
    people: BTreeMap<String, Kept<PersonReading>>,
    /// Effects handed to the session and not answered yet.
    in_flight: BTreeSet<Ask>,
}

impl SessionOrigin {
    #[must_use]
    pub fn new(link: Rc<SessionLink>) -> Self {
        Self {
            link,
            written: None,
            workspaces: BTreeMap::new(),
            tasks: BTreeMap::new(),
            people: BTreeMap::new(),
            in_flight: BTreeSet::new(),
        }
    }

    fn answered(&self) -> bool {
        let answers = self.link.answers.borrow();
        self.in_flight.iter().any(|ask| answers.contains_key(ask))
    }

    /// The writes that bring the package's resources from what was written to `reading`.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the desk's fields and its two collections"
    )]
    fn writes(
        &mut self,
        reading: &DeskReading,
        root: &ResourceRef,
        types: &[OriginType],
        shell_root: Option<&ResourceRef>,
        shell_ty: Option<&OriginType>,
    ) -> Result<Vec<OriginWrite>, String> {
        let kind = |name: &str| {
            types
                .iter()
                .find(|ty| ty.kind == format!("{ORIGIN}.{name}"))
                .ok_or_else(|| format!("the shell declares no type of `{ORIGIN}.{name}`"))
        };
        let desk = kind("desk")?;
        let field = |ty: &OriginType, name: &str| ty.field(name).map(str::to_owned);
        let text = |s: &str| Value::Text(s.to_owned());
        let was = self.written.clone().unwrap_or_default();
        let mut fields = Vec::new();
        let mut set = |name: &str, now: Value, changed: bool| {
            if (changed || self.written.is_none())
                && let Some(id) = field(desk, name)
            {
                fields.push((id, now));
            }
        };
        set(
            "person",
            text(&reading.person),
            reading.person != was.person,
        );
        set("clock", text(&reading.clock), reading.clock != was.clock);
        set(
            "weekday",
            text(&reading.weekday),
            reading.weekday != was.weekday,
        );
        set("date", text(&reading.date), reading.date != was.date);
        set(
            "unread",
            Value::Int(i64::from(reading.unread)),
            reading.unread != was.unread,
        );
        set(
            "quiet",
            Value::Bool(reading.quiet),
            reading.quiet != was.quiet,
        );
        set(
            "status_panel",
            Value::Bool(reading.status_panel),
            reading.status_panel != was.status_panel,
        );
        let mut writes = Vec::new();
        // What the screen says, in the shell's own record: the four facts a bar or dock that
        // shows only sometimes follows (see `chrome.tl`'s `Shell`). They are the shell record's
        // own fields, not the desk's, so the part writes them in the shell's root; each goes
        // only when it moved, so a still screen writes nothing.
        if let Some(root) = shell_root
            && let Some(ty) = shell_ty
        {
            let first = self.written.is_none();
            let mut screen = Vec::new();
            for (name, now, before) in [
                ("bar_reached", reading.bar_reached, was.bar_reached),
                ("bar_covered", reading.bar_covered, was.bar_covered),
                ("dock_reached", reading.dock_reached, was.dock_reached),
                ("dock_covered", reading.dock_covered, was.dock_covered),
            ] {
                if (first || now != before)
                    && let Some(id) = ty.field(name)
                {
                    screen.push((id.to_owned(), Value::Bool(now)));
                }
            }
            if !screen.is_empty() {
                writes.push(OriginWrite::Set {
                    target: root.clone(),
                    fields: screen,
                });
            }
        }
        set(
            "workspace",
            text(&reading.workspace),
            reading.workspace != was.workspace,
        );
        set("strip", text(&reading.strip), reading.strip != was.strip);
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: root.clone(),
                fields,
            });
        }
        if let (Ok(ty), Some(collection)) = (kind("workspace"), field(desk, "workspaces")) {
            writes.extend(sync_items(
                &mut self.workspaces,
                &reading.workspaces,
                root,
                &collection,
                ty,
                |w| w.key.clone(),
                |w| {
                    vec![
                        ("key", text(&w.key)),
                        ("name", text(&w.name)),
                        ("current", Value::Bool(w.current)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("task"), field(desk, "tasks")) {
            writes.extend(sync_items(
                &mut self.tasks,
                &reading.tasks,
                root,
                &collection,
                ty,
                |t| t.key.clone(),
                |t| {
                    vec![
                        ("key", text(&t.key)),
                        ("title", text(&t.title)),
                        ("state", text(&t.state)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("person"), field(desk, "people")) {
            writes.extend(sync_items(
                &mut self.people,
                &reading.people,
                root,
                &collection,
                ty,
                |p| p.key.clone(),
                |p| vec![("key", text(&p.key)), ("name", text(&p.name))],
            ));
        }
        Ok(writes)
    }
}

/// The writes that bring the items of `collection` on `root` from `kept` to `now`, by key:
/// new ones inserted (at the place the writes return, filled in by the caller), gone ones
/// removed, changed ones set. Items keep the order they were inserted in.
fn sync_items<T: Clone + PartialEq>(
    kept: &mut BTreeMap<String, Kept<T>>,
    now: &[T],
    root: &ResourceRef,
    collection: &str,
    ty: &OriginType,
    key: impl Fn(&T) -> String,
    fields: impl Fn(&T) -> Vec<(&'static str, Value)>,
) -> Vec<OriginWrite> {
    let named = |item: &T| -> Vec<(String, Value)> {
        fields(item)
            .into_iter()
            .filter_map(|(name, value)| Some((ty.field(name)?.to_owned(), value)))
            .collect()
    };
    let mut writes = Vec::new();
    let keys: BTreeSet<String> = now.iter().map(&key).collect();
    kept.retain(|k, item| {
        let stays = keys.contains(k);
        if !stays {
            writes.push(OriginWrite::Remove {
                owner: root.clone(),
                field: collection.to_owned(),
                item: item.at.clone(),
            });
        }
        stays
    });
    for item in now {
        match kept.get_mut(&key(item)) {
            Some(was) if was.was == *item => {}
            Some(was) => {
                writes.push(OriginWrite::Set {
                    target: was.at.clone(),
                    fields: named(item),
                });
                was.was = item.clone();
            }
            None => writes.push(OriginWrite::Insert {
                owner: root.clone(),
                field: collection.to_owned(),
                ty: ty.ty.clone(),
                fields: named(item),
            }),
        }
    }
    writes
}

impl SessionOrigin {
    /// Keep where the items a write inserted (`inserted`, by type and key) were made
    /// (`made`, in the writes' order), so the next reading sets or removes them there.
    fn keep_inserted(
        &mut self,
        reading: &DeskReading,
        types: &[OriginType],
        inserted: Vec<(String, String)>,
        made: Vec<Option<ResourceRef>>,
    ) {
        let mut made = made.into_iter().flatten();
        for (ty, key) in inserted {
            let Some(at) = made.next() else { break };
            let is = |name: &str| {
                types
                    .iter()
                    .any(|t| t.ty == ty && t.kind == format!("{ORIGIN}.{name}"))
            };
            if is("workspace")
                && let Some(was) = reading.workspaces.iter().find(|w| w.key == key)
            {
                self.workspaces.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("task")
                && let Some(was) = reading.tasks.iter().find(|t| t.key == key)
            {
                self.tasks.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("person")
                && let Some(was) = reading.people.iter().find(|p| p.key == key)
            {
                self.people.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            }
        }
    }
}

impl Origin for SessionOrigin {
    fn name(&self) -> &str {
        ORIGIN
    }

    fn serves(&self, capability: &str) -> bool {
        capability
            .split_once('.')
            .is_some_and(|(origin, _)| origin == ORIGIN)
    }

    fn due(&self) -> bool {
        self.link.fresh.get() || self.answered()
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let mut polled = Polled {
            wrote: false,
            settle: self.answered(),
        };
        if !self.link.fresh.replace(false) {
            return polled;
        }
        let Some(reading) = self.link.reading.borrow().clone() else {
            return polled;
        };
        let Some(types) = cx.host.find_in_packages(|package| {
            (package.meta.app == cx.app).then(|| origin_types(&package.meta, ORIGIN))
        }) else {
            tracing::error!(app = cx.app, "the shell's package is not loaded");
            return polled;
        };
        // The shell's own record (`root shell: Shell`), reached as `shell` in its views: a
        // named root of the package, made the first time it is wanted. A shell without one
        // (the bar alone, as it first shipped) writes the desk only.
        let shell_name = cx.host.find_in_packages(|package| {
            (package.meta.app == cx.app)
                .then(|| package.meta.named_roots.first().map(|root| root.name.clone()))
                .flatten()
        });
        let shell_ty = shell_name.as_ref().and_then(|name| {
            cx.host.find_in_packages(|package| {
                (package.meta.app == cx.app)
                    .then(|| {
                        let root = package
                            .meta
                            .named_roots
                            .iter()
                            .find(|root| &root.name == name)?;
                        origin_types(&package.meta, ORIGIN)
                            .into_iter()
                            .find(|ty| ty.ty == root.ty)
                    })
                    .flatten()
            })
        });
        let shell_root = shell_ty
            .as_ref()
            .and_then(|ty| cx.host.root_record(cx.app, &ty.ty).ok());
        let root = match cx.host.root_of(cx.app) {
            Ok(root) => root,
            Err(error) => {
                tracing::error!(%error, "the shell has no desk to write the session into");
                return polled;
            }
        };
        let writes = match self.writes(&reading, &root, &types, shell_root.as_ref(), shell_ty.as_ref()) {
            Ok(writes) => writes,
            Err(why) => {
                tracing::error!(why, "the session's desk does not fit the shell");
                return polled;
            }
        };
        let inserted: Vec<(String, String)> = writes
            .iter()
            .filter_map(|write| match write {
                OriginWrite::Insert { ty, fields, .. } => {
                    let key = types
                        .iter()
                        .find(|t| t.ty == *ty)
                        .and_then(|t| t.field("key"))
                        .and_then(|id| fields.iter().find(|(f, _)| f == id))
                        .and_then(|(_, v)| match v {
                            Value::Text(key) => Some(key.clone()),
                            _ => None,
                        })?;
                    Some((ty.clone(), key))
                }
                _ => None,
            })
            .collect();
        match cx
            .host
            .runtime()
            .apply_origin(ORIGIN, writes, Some(cx.now.clone()))
        {
            Ok(made) => {
                self.keep_inserted(&reading, &types, inserted, made);
                self.written = Some(reading);
                polled.wrote = true;
            }
            Err(error) => {
                // Nothing was written: the next reading tries again from what was.
                tracing::error!(%error, "the session's desk could not be written");
            }
        }
        polled
    }

    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        let ask = match ask_of(capability, payload) {
            Ok(ask) => ask,
            Err(why) => return Delivery::Rejected(why),
        };
        if let Some(answer) = self.link.answers.borrow_mut().remove(&ask) {
            self.in_flight.remove(&ask);
            return match answer {
                Ok(()) => Delivery::Delivered,
                Err(why) => Delivery::Rejected(why),
            };
        }
        if self.in_flight.insert(ask.clone()) {
            self.link.asked.borrow_mut().push_back(ask);
            self.link.handed.set(self.link.handed.get().wrapping_add(1));
        }
        // Answered by the session's next turn ([`SessionLink::serve`]), which the poll that
        // takes the answer in settles.
        Delivery::Pending
    }
}

/// What a shell's instance asks of the session's process.
struct ShellPlatform {
    packages: Rc<Vec<Compiled>>,
    waker: Waker,
}

impl Platform for ShellPlatform {
    fn load(&self, host: &Host) -> u64 {
        for compiled in self.packages.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                tracing::error!(app = compiled.meta.app, %error, "a shell package did not load");
            }
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        let args: Vec<(&str, ui::i18n::ArgValue)> = args
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str().into()))
            .collect();
        tilo_ui_host::i18n::t_args(id, &args)
    }

    fn now(&self) -> String {
        ui::clock::local_now()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::clone(&self.waker)
    }
}

/// The person at the device, as the shell names them.
struct Person(String);

impl NameHost for Person {
    fn name(&self, who: &Principal) -> String {
        if who.0.is_empty() {
            self.0.clone()
        } else {
            who.0.clone()
        }
    }
}

/// The room a shell's record is decided in: the person's own, on this device.
const SHELL_ROOM: &str = "shell";

/// `person`'s standing over their own shell `app`: they own its record, and their shell may
/// hand the session its outside effects (`session.*`) without asking each time, as their
/// chosen shell. The grant is theirs, in the one policy every door decides with.
fn shell_standing(
    app: &str,
    person: &str,
) -> impl Fn() -> Option<tilo_action_policy::Standing> + 'static {
    use tilo_action_policy::{Answer, Grant, KindSet, RoomPolicy, Standing, Subject, TreePath};
    let mut policy = RoomPolicy::new(SHELL_ROOM, person);
    policy.grants.push(Grant::new(
        format!("shell:{app}:session"),
        Subject::app(app),
        TreePath::room(SHELL_ROOM).app(app),
        KindSet::OUTSIDE,
        Answer::Allow,
        Subject::person(person),
    ));
    let standing = Standing::person(policy, person);
    move || Some(standing.clone())
}

/// How much work the part did: what its idle and its frames cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellWork {
    /// Frames in which the instance ran (something had changed).
    pub turns: u64,
    /// Frames the toolkit drew and the CPU rasterized.
    pub drawn: u64,
    /// Time the instance and the window spent answering each other (the view run, its
    /// patches, the widgets reconciled) and the layout after it.
    pub turning: std::time::Duration,
    /// Time the toolkit spent drawing those frames and the CPU rasterizing them.
    pub drawing: std::time::Duration,
    /// Of [`Self::turning`]: the instance's updates (views run, patches made), the commits
    /// to the shell's record, the window taking the patches in, and the layout after.
    pub updating: std::time::Duration,
    pub committing: std::time::Duration,
    pub receiving: std::time::Duration,
    pub laying_out: std::time::Duration,
    /// Of [`Self::committing`]: the instance reading the record anew after a commit.
    pub syncing: std::time::Duration,
    /// Effects the shell handed the session's door, and those the session ran.
    pub handed: u64,
    pub served: u64,
}

/// One part of a tilolang shell on the desktop. See the module docs.
pub struct ShellPart {
    widgets: WidgetHost,
    instance: Instance,
    record: tilo_resource::Doc,
    origin: Rc<RefCell<SessionOrigin>>,
    /// Input reached the part since its last turn.
    input: bool,
    /// The last turn changed something: the next looks again, for what that started.
    again: bool,
    work: ShellWork,
    /// What the shell refused, in its words.
    refusals: Vec<String>,
    /// How high the part stands at rest (the bar's band), logical pixels.
    band: u32,
    /// The outputs' bounding box `(x, y, w, h)` the part spans.
    outputs: Option<(i32, i32, u32, u32)>,
    /// A menu the part opened is up: the part covers the outputs so it has room. At rest it
    /// covers only its bands, so a change draws only that.
    menu: bool,
    /// The edges the shell's chrome stands on: what the part covers of the outputs.
    edges: HostEdges,
}

impl ShellPart {
    /// The root view of `app` (the last of `packages`, which come in load order) for `person`,
    /// across the top `band` logical pixels of the outputs at `scale`, kept in step with the
    /// session through `link`; `waker` wakes the session's loop when work the instance waits
    /// for is ready.
    #[must_use]
    pub fn new(
        app: &str,
        packages: Vec<Compiled>,
        person: &str,
        link: Rc<SessionLink>,
        band: u32,
        scale: f64,
        waker: Waker,
    ) -> Self {
        let placement = Placement {
            origin: (0, 0),
            size: (1, band.max(1)),
            scale,
        };
        tilo_ui_host::i18n::ensure();
        let (app_end, window_end) = pair();
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: person.to_owned(),
                mode: Mode::Live,
                device: None,
                index_dir: None,
                record_loader: None,
                sink: None,
            },
            Rc::new(ShellPlatform {
                packages: Rc::new(packages),
                waker,
            }),
            Rc::new(Person(person.to_owned())),
            Box::new(app_end),
        );
        instance.set_standing(Rc::new(shell_standing(app, person)));
        let origin = Rc::new(RefCell::new(SessionOrigin::new(link)));
        instance.add_origin(Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin);
        let record = tilo_resource::Doc::new();
        instance.sync(Some(&record));
        let window = AppWindow::part(Box::new(window_end));
        let widgets = WidgetHost::new(Box::new(window), placement, false);
        let mut part = Self {
            widgets,
            instance,
            record,
            origin,
            input: false,
            again: true,
            work: ShellWork::default(),
            refusals: Vec::new(),
            band: band.max(1),
            outputs: None,
            menu: false,
            edges: HostEdges {
                bar: true,
                bar_top: true,
                dock: false,
                dock_bottom: false,
            },
        };
        part.seed();
        part.turn();
        part
    }

    /// Make the shell's own record (`root shell: Shell`) and let the package's migration seed
    /// it — the bar's parts and the dock's items, on a shell never arranged — as the package
    /// version's change, as an app's migration is. Both go through the part's own turns: the
    /// record is created as a queued change, the migration runs once the record is had, and
    /// [`Self::commit`] writes each into the part's record.
    fn seed(&mut self) {
        let Some(host) = self.instance.host() else {
            return;
        };
        let roots = host
            .package(self.instance.app())
            .map(|package| package.meta.named_roots.clone())
            .unwrap_or_default();
        for root in &roots {
            if let Err(error) = host.root_record(self.instance.app(), &root.ty) {
                tracing::warn!(%error, "the shell's record could not be made");
                return;
            }
        }
        if roots.is_empty() {
            return;
        }
        // The package's migrations run for the room's origin alone, as an app's do. Each
        // committed action is written into the part's record, as the door writes an app's: the
        // record's making first, then the seed that runs once the record is had.
        self.instance.set_origin(true);
        for _ in 0..3 {
            self.instance.sync(Some(&self.record));
            self.instance.commit_planned();
            if !self.commit() {
                break;
            }
        }
    }

    /// Stand over the outputs: the whole of them while a menu of the part's is up, and else
    /// over each edge the shell's bar and dock ride — a bar at the top takes the top band, a
    /// dock at the bottom the bottom one — through `edges`. A shell whose chrome is off at an
    /// edge, or is a bar at the top alone (the first shell), takes that band only.
    fn place(&mut self, edges: HostEdges) {
        let Some((x, y, width, output_height)) = self.outputs else {
            return;
        };
        let scale = self.widgets.placement().scale;
        let band = self.band.min(output_height);
        if self.menu {
            self.widgets.set_placement(Placement {
                origin: (x, y),
                size: (width.max(1), output_height.max(1)),
                scale,
            });
            return;
        }
        let at_top = edges.bar_at_top();
        let at_bottom = edges.dock_at_bottom();
        // The chrome at one edge alone stands in a band there. Both, or neither, and the part
        // covers the outputs, so each band is where it is.
        let height = match (at_top, at_bottom) {
            (true, false) | (false, true) => band,
            _ => output_height.max(band),
        };
        let origin_y = if at_bottom && !at_top {
            y.saturating_add(
                i32::try_from(output_height.saturating_sub(height)).unwrap_or(i32::MAX),
            )
        } else {
            y
        };
        self.widgets.set_placement(Placement {
            origin: (x, origin_y),
            size: (width.max(1), height.max(1)),
            scale,
        });
    }

    /// Follow whether a menu the part opened is up, growing over the outputs for it and back
    /// to the band once it closed.
    fn follow_menu(&mut self) {
        let menu = self
            .widgets
            .host_mut()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.role() == ui::accesskit::Role::Menu);
        if menu != self.menu {
            self.menu = menu;
            self.place(self.edges);
        }
    }

    /// How much work the part did since it started.
    #[must_use]
    pub fn work(&self) -> ShellWork {
        let origin = self.origin.borrow();
        ShellWork {
            handed: origin.link.handed.get(),
            served: origin.link.served.get(),
            ..self.work
        }
    }

    /// Where the control assistive technology names `name` stands, in the desktop's logical
    /// layout `(x, y, width, height)`: what a test or a tool presses it by.
    pub fn control_bounds(&mut self, name: &str) -> Option<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some(name))
            .find_map(|(_, node)| node.bounds())
            .map(|rect| {
                (
                    f64::from(ox) + rect.x0,
                    f64::from(oy) + rect.y0,
                    rect.width(),
                    rect.height(),
                )
            })
    }

    /// The edges the shell's chrome stands on, as the session read them from the shell's
    /// record: where the part covers the outputs. A change moves the part to the edges it
    /// now names.
    pub fn set_edges(&mut self, edges: HostEdges) {
        if edges == self.edges {
            return;
        }
        self.edges = edges;
        self.place(edges);
    }

    /// The shell's own record (`root shell: Shell`) as it holds it now, when the package has
    /// one and the record is made: what its edge settings are read from.
    #[must_use]
    pub fn shell_record(&self) -> Option<tilolang_tilo::record::DynRecord> {
        let host = self.instance.host()?;
        let package = host.package(self.instance.app())?;
        let root = package.meta.named_roots.first()?;
        host.store().record(&format!("root-{}", root.ty)).ok()
    }

    /// The package the part shows, as it was compiled: its types and their fields.
    #[must_use]
    pub fn package(&self) -> Option<tilolang_tilo::host::Package> {
        self.instance.host()?.package(self.instance.app())
    }

    /// The toolkit's host of the part's widget tree, to drive it directly.
    pub fn widgets_mut(&mut self) -> &mut WidgetHost {
        &mut self.widgets
    }

    fn window(&mut self) -> Option<&mut AppWindow> {
        self.widgets
            .host_mut()
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Commit what the instance planned to the shell's record, as the door would, and bring
    /// the instance up to date with it. Whether anything was committed.
    fn commit(&mut self) -> bool {
        let mut committed = false;
        while let Some((invocation, _)) = self.instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource != HOST_RESOURCE || address.action != COMMIT {
                self.instance.answered(
                    &address,
                    Some("a shell part runs only its own commits".to_owned()),
                );
                continue;
            }
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            let applied = self
                .instance
                .host()
                .and_then(|host| host.store().take_ticket(ticket))
                .ok_or_else(|| format!("no committed action {ticket}"))
                .and_then(|action| {
                    action
                        .apply(&mut self.record)
                        .map_err(|error| error.to_string())
                });
            if let Err(why) = &applied {
                tracing::error!(why, "a shell change could not be committed");
            }
            self.instance.answered(&address, applied.err());
            committed = true;
        }
        if committed {
            // The record holds the changes once they are committed: the instance reads them
            // from its heads.
            self.record.commit();
            let at = std::time::Instant::now();
            self.instance.sync(Some(&self.record));
            self.work.syncing = self.work.syncing.saturating_add(at.elapsed());
        }
        committed
    }

    /// Let the window and the instance answer each other until neither has more to say.
    /// Whether the window took anything.
    fn turn(&mut self) -> bool {
        let started = std::time::Instant::now();
        self.work.turns = self.work.turns.wrapping_add(1);
        let mut shown = false;
        let mut quiet = false;
        let lap = |since: &mut std::time::Instant| {
            let now = std::time::Instant::now();
            let took = now.saturating_duration_since(*since);
            *since = now;
            took
        };
        for _ in 0..EXCHANGES {
            let mut at = std::time::Instant::now();
            if let Some(window) = self.window() {
                window.flush();
            }
            let updated = self.instance.update();
            self.work.updating = self.work.updating.saturating_add(lap(&mut at));
            let committed = self.commit();
            self.work.committing = self.work.committing.saturating_add(lap(&mut at));
            let received = self.window().is_some_and(AppWindow::receive);
            self.hear_notices();
            self.work.receiving = self.work.receiving.saturating_add(lap(&mut at));
            shown |= received;
            if !(updated || committed || received) {
                quiet = true;
                break;
            }
        }
        if shown {
            let at = std::time::Instant::now();
            self.widgets.relayout();
            self.work.laying_out = self.work.laying_out.saturating_add(at.elapsed());
        }
        // The exchanges ran until neither side had more to say, unless they ran out first:
        // then the next frame goes on.
        self.again = !quiet;
        self.work.turning = self.work.turning.saturating_add(started.elapsed());
        shown
    }

    /// What the shell's views said that a part has no window to show: a refusal is the
    /// session's to hear and is kept for whoever asks ([`Self::refusals`]).
    fn hear_notices(&mut self) {
        let Some(window) = self.window() else {
            return;
        };
        for (kind, text) in window.take_notices() {
            match kind {
                tilo_render_protocol::patch::NoticeKind::Error => {
                    tracing::warn!(text, "the shell refused what was asked of it");
                    self.refusals.push(text);
                }
                tilo_render_protocol::patch::NoticeKind::Info => {
                    tracing::info!(text, "the shell says");
                }
            }
        }
    }

    /// What the shell refused since it started, in its own words.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refusals
    }

    fn due(&self) -> bool {
        self.input || self.again || self.origin.borrow().due()
    }
}

impl crate::Published for ShellPart {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        crate::Published::resources(&mut self.widgets)
    }

    fn revision(&self) -> Option<u64> {
        crate::Published::revision(&self.widgets)
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        self.input = true;
        crate::Published::invoke(&mut self.widgets, address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        crate::Published::accessible_labels(&mut self.widgets)
    }
}

impl SurfaceHost for ShellPart {
    fn layers(&self) -> Vec<HostLayer> {
        self.widgets.layers()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        // While a menu of the part's is up, a press anywhere over it is the part's: outside
        // the menu it closes it, as a press outside any menu does.
        if self.menu
            && let Some((ox, oy, width, height)) = self.outputs
        {
            let (x, y) = (x - f64::from(ox), y - f64::from(oy));
            return x >= 0.0 && y >= 0.0 && x < f64::from(width) && y < f64::from(height);
        }
        self.widgets.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.input = true;
        self.widgets.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.input = true;
        self.widgets.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input = true;
        self.widgets.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.input = true;
        self.widgets.pointer_axis(horizontal, vertical);
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        let Some((x0, y0, x1, y1)) = outputs
            .iter()
            .map(|&(x, y, w, h)| (x, y, x.saturating_add(w), y.saturating_add(h)))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (
            u32::try_from(x1.saturating_sub(x0)),
            u32::try_from(y1.saturating_sub(y0)),
        ) else {
            return;
        };
        self.outputs = Some((x0, y0, width, height));
        self.place(self.edges);
    }

    fn wants_keyboard(&self) -> bool {
        self.widgets.wants_keyboard()
    }

    fn key(&mut self, key: &HostKey) {
        self.input = true;
        self.widgets.key(key);
    }

    fn cursor(&self) -> CursorIcon {
        self.widgets.cursor()
    }

    fn prepare_frame(&mut self) {
        let input = std::mem::take(&mut self.input);
        if input || self.due() {
            self.turn();
        }
        self.draw();
        // The toolkit takes input in at its frame: what the person pressed reports there, and
        // the instance hears of it now rather than at the next input.
        if input {
            if self.turn() {
                self.draw();
            }
            // Only input opens or closes a menu.
            let menu = self.menu;
            self.follow_menu();
            if self.menu != menu {
                self.draw();
            }
        }
    }
}

impl ShellPart {
    /// Draw a frame of the part's widget tree when it owes one.
    fn draw(&mut self) {
        let before = crate::Published::revision(&self.widgets);
        let started = std::time::Instant::now();
        self.widgets.prepare_frame();
        if crate::Published::revision(&self.widgets) != before {
            self.work.drawn = self.work.drawn.wrapping_add(1);
            self.work.drawing = self.work.drawing.saturating_add(started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        reason = "test fixtures"
    )]

    use super::*;

    /// The shell package's files, as the session compiles them: every `.tl` of the package is
    /// part of its one module.
    const FILES: [(&str, &str); 5] = [
        ("app.tl", include_str!("../../../../apps/tl/shell/app.tl")),
        ("chrome.tl", include_str!("../../../../apps/tl/shell/chrome.tl")),
        ("docks.tl", include_str!("../../../../apps/tl/shell/docks.tl")),
        ("layout.tl", include_str!("../../../../apps/tl/shell/layout.tl")),
        (
            "system.tl",
            include_str!("../../../../apps/tl/shell/system.tl"),
        ),
    ];
    const IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");

    fn reading() -> DeskReading {
        DeskReading {
            person: "Pat".to_owned(),
            clock: "4:28 PM".to_owned(),
            workspace: "Desk".to_owned(),
            strip: "Main".to_owned(),
            workspaces: vec![
                WorkspaceReading {
                    key: "1".to_owned(),
                    name: "Desk".to_owned(),
                    current: true,
                },
                WorkspaceReading {
                    key: "2".to_owned(),
                    name: "Studio".to_owned(),
                    current: false,
                },
            ],
            ..DeskReading::default()
        }
    }

    fn part() -> (ShellPart, Rc<SessionLink>) {
        let lock = tilolang_tilo::tilolang::IdsLock::parse(IDS).unwrap();
        let source = tilolang_tilo::tilolang::PackageSource::new(
            FILES
                .into_iter()
                .map(|(path, text)| (format!("apps/tl/shell/{path}"), text.to_owned())),
        );
        let compiled =
            tilolang_tilo::tilolang::compile_package_source(&source, &lock, &[]).unwrap();
        let app = compiled.meta.app.clone();
        let link = Rc::new(SessionLink::default());
        link.publish(reading());
        let mut part = ShellPart::new(
            &app,
            vec![compiled],
            "Pat",
            Rc::clone(&link),
            32,
            1.0,
            std::sync::Arc::new(|| {}),
        );
        part.outputs_changed(&[(0, 0, 1280, 800)]);
        for _ in 0..4 {
            part.prepare_frame();
        }
        (part, link)
    }

    fn click(part: &mut ShellPart, name: &str) {
        let (x, y, w, h) = part
            .control_bounds(name)
            .unwrap_or_else(|| panic!("the bar shows {name}"));
        part.pointer_motion(x + w / 2.0, y + h / 2.0);
        part.prepare_frame();
        part.pointer_button(0x110, true);
        part.prepare_frame();
        part.pointer_button(0x110, false);
        for _ in 0..6 {
            part.prepare_frame();
        }
    }

    #[test]
    fn a_press_on_the_bar_hands_its_effect_to_the_session() {
        let (mut part, link) = part();
        click(&mut part, "Notifications");
        let outbox = part
            .instance
            .host()
            .map(|host| host.store().outbox_pending_count());
        assert!(
            link.has_asks(),
            "the press reached the session: outbox {outbox:?}, {:?}, refusals {:?}",
            part.work(),
            part.refusals()
        );
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.action.clone()),
            Some("toggle-quick-settings".to_owned())
        );
    }

    /// What one discrete change costs the bar: printed per kind of change, for the record
    /// of its frame budget; each must land in one turn.
    #[test]
    fn each_change_is_one_turn() {
        type Change = (&'static str, fn(&mut DeskReading));
        let (mut part, link) = part();
        for _ in 0..4 {
            part.prepare_frame();
        }
        let mut now = reading();
        let changes: [Change; 3] = [
            ("minute", |r| r.clock = "4:29 PM".to_owned()),
            ("notification", |r| r.unread = 1),
            ("workspace switch", |r| {
                r.workspace = "Studio".to_owned();
                r.workspaces[0].current = false;
                r.workspaces[1].current = true;
            }),
        ];
        for (name, change) in changes {
            change(&mut now);
            link.publish(now.clone());
            let before = part.work();
            part.prepare_frame();
            part.prepare_frame();
            let after = part.work();
            let ms = |a: std::time::Duration, b: std::time::Duration| {
                a.saturating_sub(b).as_secs_f64() * 1000.0
            };
            println!(
                "SHELL-CHANGE {name}: {} turns, {:.3} ms (update {:.3}, commit {:.3} of which sync {:.3}, receive {:.3}, layout {:.3}), {} drawn {:.3} ms",
                after.turns - before.turns,
                ms(after.turning, before.turning),
                ms(after.updating, before.updating),
                ms(after.committing, before.committing),
                ms(after.syncing, before.syncing),
                ms(after.receiving, before.receiving),
                ms(after.laying_out, before.laying_out),
                after.drawn - before.drawn,
                ms(after.drawing, before.drawing),
            );
            assert_eq!(after.turns - before.turns, 1, "{name} is one turn");
        }
    }

    #[test]
    fn the_part_covers_its_band_at_rest_and_the_outputs_while_its_menu_is_up() {
        let (mut part, _link) = part();
        assert_eq!(
            part.widgets.placement().size,
            (1280, 32),
            "the band at rest"
        );
        assert!(
            !part.accepts_pointer(900.0, 400.0),
            "the desk under it takes presses"
        );
        click(&mut part, "Desk");
        assert_eq!(
            part.widgets.placement().size,
            (1280, 800),
            "room for the menu"
        );
        assert!(part.control_bounds("Studio").is_some(), "the menu is up");
        // A press away from the menu closes it.
        assert!(part.accepts_pointer(900.0, 400.0));
        part.pointer_motion(900.0, 400.0);
        part.prepare_frame();
        part.pointer_button(0x110, true);
        part.prepare_frame();
        part.pointer_button(0x110, false);
        for _ in 0..4 {
            part.prepare_frame();
        }
        assert!(part.control_bounds("Studio").is_none(), "the menu closed");
        assert_eq!(
            part.widgets.placement().size,
            (1280, 32),
            "back to the band"
        );
    }

    #[test]
    fn choosing_a_workspace_in_the_menu_asks_the_session_to_switch() {
        let (mut part, link) = part();
        click(&mut part, "Desk");
        click(&mut part, "Studio");
        let asked = link.asked.borrow().front().cloned();
        assert_eq!(
            asked.map(|ask| (ask.action, ask.arguments)),
            Some((
                SWITCH_WORKSPACE.to_owned(),
                BTreeMap::from([(WORKSPACE_ARG.to_owned(), "2".to_owned())])
            )),
            "{:?} {:?}",
            part.work(),
            part.refusals()
        );
        // The session ran it: the effect is settled.
        link.serve(|_| Ok(()));
        for _ in 0..4 {
            part.prepare_frame();
        }
        assert!(!link.has_asks());
        assert!(
            part.origin.borrow().in_flight.is_empty(),
            "the effect was answered"
        );
    }

    /// Whether the bar's bell says `words` among the part's accessible labels: what a person
    /// hears of the bar's status part.
    fn says(part: &mut ShellPart, words: &str) -> bool {
        crate::Published::accessible_labels(&mut part.widgets)
            .iter()
            .any(|label| label == words)
    }

    /// The root of the person's shell record (`root shell: Shell`), made now.
    fn shell_root(part: &ShellPart) -> ResourceRef {
        let ty = part
            .instance
            .host()
            .and_then(|host| {
                host.package(part.instance.app())?
                    .meta
                    .named_roots
                    .first()
                    .map(|root| root.ty.clone())
            })
            .expect("the shell declares its record as a named root");
        part.instance
            .host()
            .expect("the part runs a host")
            .root_record(part.instance.app(), &ty)
            .expect("the shell record is made")
    }

    /// Set one of the shell record's facts as the session's screen reading does: a register on
    /// the record, through the origin write the session's origin applies.
    fn screen(part: &mut ShellPart, field: &str, on: bool) {
        let id = part
            .package()
            .into_iter()
            .flat_map(|package| package.meta.types)
            .find(|ty| ty.name == "Shell")
            .and_then(|ty| ty.fields.into_iter().find(|f| f.name == field))
            .map_or_else(|| panic!("the record has no {field}"), |f| f.id);
        let writes = vec![OriginWrite::Set {
            target: shell_root(part),
            fields: vec![(id, Value::Bool(on))],
        }];
        part.instance
            .host()
            .expect("the part runs a host")
            .runtime()
            .apply_origin("session", writes, None)
            .expect("the screen's facts are the session's to write");
        // The write is a change of the record, as the session's origin's write is: the part
        // commits it and runs its views again.
        part.instance.commit_planned();
        assert!(part.commit(), "the screen's fact is committed to the record");
        part.instance.moved();
        part.turn();
    }

    /// The person chooses the preset `preset`, as their settings' card does: the action runs
    /// in the part's runtime, and the part commits it to its record.
    fn choose_preset(part: &mut ShellPart, preset: &str) {
        let shell = shell_root(part);
        let meta = part
            .instance
            .host()
            .unwrap()
            .package(part.instance.app())
            .unwrap()
            .meta;
        let apply = meta
            .actions
            .iter()
            .find(|a| a.name == "apply")
            .expect("the shell's apply action")
            .id
            .clone();
        let preset_enum = meta
            .enums
            .iter()
            .find(|en| en.name == "Preset")
            .expect("the shell declares its Preset");
        let preset_ty = preset_enum.id.clone();
        let variant = preset_enum
            .variants
            .iter()
            .find(|v| v.name == preset)
            .unwrap_or_else(|| panic!("no preset {preset}"))
            .id
            .clone();
        part.instance
            .host()
            .unwrap()
            .runtime()
            .invoke_action(
                &tilolang_tilo::runtime::Principal::new("Pat"),
                (
                    apply.as_str(),
                    vec![
                        Value::resource(&shell),
                        Value::Enum(tilolang_tilo::runtime::value::EnumValue {
                            ty: preset_ty,
                            variant,
                            fields: std::collections::BTreeMap::new(),
                        }),
                    ],
                ),
            )
            .expect("the preset applies");
        part.instance.commit_planned();
        assert!(part.commit(), "the preset is committed to the record");
        // The record moved: the views run again on the part's next turns.
        part.instance.moved();
        part.turn();
    }

    /// The person sets one of the shell's enum settings (`set_bar_showing` and the like) to
    /// `case`, as a button in the settings does: the action runs and the part commits it.
    fn showing(part: &mut ShellPart, action: &str, case: &str) {
        let shell = shell_root(part);
        let meta = part
            .instance
            .host()
            .unwrap()
            .package(part.instance.app())
            .unwrap()
            .meta;
        let id = meta
            .actions
            .iter()
            .find(|a| a.name == action)
            .unwrap_or_else(|| panic!("no action {action}"))
            .id
            .clone();
        let param_ty = &meta
            .actions
            .iter()
            .find(|a| a.name == action)
            .unwrap()
            .params[1]
            .ty;
        let enum_name = param_ty.trim_start_matches("enum ");
        let en = meta
            .enums
            .iter()
            .find(|en| en.name == enum_name)
            .unwrap_or_else(|| panic!("no enum {enum_name}"));
        let variant = en
            .variants
            .iter()
            .find(|v| v.name == case)
            .unwrap_or_else(|| panic!("no {enum_name}.{case}"))
            .id
            .clone();
        let ty = en.id.clone();
        part.instance
            .host()
            .unwrap()
            .runtime()
            .invoke_action(
                &tilolang_tilo::runtime::Principal::new("Pat"),
                (
                    id.as_str(),
                    vec![
                        Value::resource(&shell),
                        Value::Enum(tilolang_tilo::runtime::value::EnumValue {
                            ty,
                            variant,
                            fields: std::collections::BTreeMap::new(),
                        }),
                    ],
                ),
            )
            .expect("the setting applies");
        part.instance.commit_planned();
        assert!(part.commit(), "the setting is committed to the record");
        part.instance.moved();
        part.turn();

    }

    #[test]
    fn a_bar_that_hides_until_reached_for_shows_when_the_screen_says_it_is_reached() {
        let (mut part, _link) = part();
        for _ in 0..4 {
            part.prepare_frame();
        }
        // The default shell shows its bar always: the bell is there.
        assert!(says(&mut part, "Notifications"), "the bar shows at rest");
        // The person chooses the minimal layout: a bar that hides until reached for.
        choose_preset(&mut part, "minimal");
        assert!(
            !says(&mut part, "Notifications"),
            "a minimal shell has no bar at rest"
        );
        // The pointer comes to the top edge: the session says it is reached.
        screen(&mut part, "bar_reached", true);
        assert!(
            says(&mut part, "Notifications"),
            "the bar shows when the pointer bumps its edge"
        );
        // It leaves: the bar hides again.
        screen(&mut part, "bar_reached", false);
        assert!(
            !says(&mut part, "Notifications"),
            "and hides when the pointer leaves"
        );
    }

    #[test]
    fn a_bar_that_hides_while_a_window_overlaps_it_hides_only_while_one_does() {
        let (mut part, _link) = part();
        for _ in 0..4 {
            part.prepare_frame();
        }
        // The person's bar shows only while no window overlaps it.
        showing(&mut part, "set_bar_showing", "no_overlap");
        assert!(says(&mut part, "Notifications"), "the bar shows at rest");
        // A window stands where the bar rides: the session says the top edge is covered.
        screen(&mut part, "bar_covered", true);
        assert!(
            !says(&mut part, "Notifications"),
            "the bar hides while a window covers it"
        );
        screen(&mut part, "bar_covered", false);
        assert!(says(&mut part, "Notifications"), "and shows once it is clear");
    }

    #[test]
    fn the_part_stands_in_the_band_the_shell_records_dock_names() {
        let (mut part, _link) = part();
        assert_eq!(
            part.widgets.placement().origin,
            (0, 0),
            "the first shell: a bar at the top"
        );
        part.set_edges(HostEdges {
            bar: false,
            bar_top: true,
            dock: true,
            dock_bottom: true,
        });
        assert_eq!(
            part.widgets.placement().origin.1,
            800 - 32,
            "a dock at the bottom alone stands in the bottom band"
        );
        assert_eq!(part.widgets.placement().size, (1280, 32));
    }
}
