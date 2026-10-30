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
}

/// What a shell asked of the session: an invocation on the door.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ask {
    pub resource: String,
    pub action: String,
    pub arguments: BTreeMap<String, String>,
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
        set(
            "workspace",
            text(&reading.workspace),
            reading.workspace != was.workspace,
        );
        set("strip", text(&reading.strip), reading.strip != was.strip);
        let mut writes = Vec::new();
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
        let Some(package) = cx.host.package(cx.app) else {
            tracing::error!(app = cx.app, "the shell's package is not loaded");
            return polled;
        };
        let types = origin_types(&package.meta, ORIGIN);
        let root = match cx.host.root_of(cx.app) {
            Ok(root) => root,
            Err(error) => {
                tracing::error!(%error, "the shell has no desk to write the session into");
                return polled;
            }
        };
        let writes = match self.writes(&reading, &root, &types) {
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
                    }
                }
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
    /// Time spent in those turns and draws.
    pub busy: std::time::Duration,
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
    /// covers only its band, so a change draws only that.
    menu: bool,
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
        };
        part.turn();
        part
    }

    /// Stand over the outputs: the whole of them while a menu of the part's is up, else the
    /// band.
    fn place(&mut self) {
        let Some((x, y, width, height)) = self.outputs else {
            return;
        };
        let scale = self.widgets.placement().scale;
        let height = if self.menu {
            height
        } else {
            self.band.min(height)
        };
        self.widgets.set_placement(Placement {
            origin: (x, y),
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
            self.place();
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
            self.instance.sync(Some(&self.record));
        }
        committed
    }

    /// Let the window and the instance answer each other until neither has more to say.
    /// Whether the window took anything.
    fn turn(&mut self) -> bool {
        let started = std::time::Instant::now();
        self.work.turns = self.work.turns.wrapping_add(1);
        let mut shown = false;
        let mut moved = false;
        for _ in 0..EXCHANGES {
            if let Some(window) = self.window() {
                window.flush();
            }
            let updated = self.instance.update();
            let committed = self.commit();
            let received = self.window().is_some_and(AppWindow::receive);
            self.hear_notices();
            shown |= received;
            moved |= updated || committed || received;
            if !(updated || committed || received) {
                break;
            }
        }
        if shown {
            self.widgets.relayout();
        }
        self.again = moved;
        self.work.busy = self.work.busy.saturating_add(started.elapsed());
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
        self.place();
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
            self.work.busy = self.work.busy.saturating_add(started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

    use super::*;

    const SOURCE: &str = include_str!("../../../../apps/tl/shell/app.tl");
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
        let compiled = tilolang_tilo::tilolang::compile_package(SOURCE, &lock, &[]).unwrap();
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
}
