//! The shell (`so.tilo.shell`) as a view a device shows where no desktop session runs it: a web
//! page of the workspace's desktop (`/w/<workspace>/desktop`).
//!
//! A session runs its shell in its own process and writes it the person's layout, the apps and
//! the windows. Here the shell is an instance like any app's, and what it reads of a session it
//! is handed from what the device's dispatch answers, asked as the person at the page:
//!
//! - **The layout** is the workspace's shared layout, read through its `read` action
//!   (`so.tilo.layout`), and written into the shell's `session.*` resources as a session writes
//!   it ([`LayoutOrigin`]). It is read again while the page is open, so a window someone opened
//!   or closed anywhere shows here.
//! - **The apps** the launcher lists (`session:apps`) are those the dispatch's catalogue lists the
//!   person, and **the windows** the dock and the switcher walk (`session:windows`) are the
//!   layout's.
//! - **The styles** the person's switch lists (`styles.installed`) are the ones the device
//!   publishes in the person's own workspace, read as the person at the page and followed by
//!   revision, so a style installed while the page is open is offered there too. Picking one is
//!   the shell's own `show_style`, which the dispatch runs over the person's shell record.
//! - **What the views ask of the session** is carried out where its state is: a window's Close
//!   and the launcher's Open are the layout's `close` and `place`, as the person at the page;
//!   the surface over the windows (`desktop:overlays`) is the page's own, as a device's shell
//!   keeps its own, so nobody else's screen moves when this person opens the launcher.
//!
//! What else a view asks of a session goes through the dispatch as any call does, and is
//! answered as it is there.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use collaboration::{CollabEvent, CollabRequest};
use serde_json::{Value, json};
use tilo_actions::remote::{
    CatalogueQuery, InvocationReceipt, InvokeArgument, InvokeRequest, ReadAnswer, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget, ResourceReadRequest,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::shell::{LayoutOrigin, LayoutReading, LayoutWorkspaceReading};
use ui_core::time::{Duration, Instant};

/// The shell's package id: the app whose instances are shown this way.
pub const SHELL_APP: &str = tilolang_tilo::shell::SHELL_APP;

/// The provider and resource of the workspace's layout's actions, as the device publishes them.
const LAYOUT_PROVIDER: &str = "so.tilo.layout";
const LAYOUT_RESOURCE: &str = "workspace:layout";

/// The session's app, whose arranging actions the page carries out on the shared layout.
const SESSION_APP: &str = tilolang_tilo::platform_apps::SESSION_APP;

/// The app the device publishes the styles at, and the platform root the shell reads them by.
const STYLES_APP: &str = tilolang_tilo::platform_apps::STYLES_APP;

/// The device's app, whose `show-surface` the page carries out on its own overlays.
const DEVICE_APP: &str = tilolang_tilo::platform_apps::DEVICE_APP;

/// The surface over the windows, as the session publishes it.
const OVERLAYS: &str = "desktop:overlays";

/// What the session publishes a place as (`shell:place:<key>`), its key being the window's.
const PLACE: &str = "shell:place:";

/// How often the layout and the apps are read again while the page is open.
const READ_AGAIN: Duration = Duration::from_millis(1000);

/// How long a read may go unanswered before it is asked again.
const READ_LOST: Duration = Duration::from_secs(10);

/// The most apps the launcher lists.
const MOST_APPS: u32 = 200;

/// How long a follow of the styles waits for them to move before it is answered as they stand:
/// within [`READ_LOST`], so a follow that waits is never taken for one that was lost.
const STYLES_WAIT_MS: u32 = 5_000;

/// What the shell's views asked that the page carries out, as they asked it.
type Asked = Rc<RefCell<Vec<Invocation>>>;

/// One read in flight or due: the layout, or the apps.
#[derive(Default)]
struct Read {
    asked_at: Option<Instant>,
    waiting: bool,
    /// Asked for at once, whatever its last read: something it reads just changed.
    now: bool,
}

impl Read {
    fn due(&self, now: Instant) -> bool {
        match self.asked_at {
            None => true,
            Some(at) if self.waiting => now.saturating_duration_since(at) >= READ_LOST,
            Some(_) if self.now => true,
            Some(at) => now.saturating_duration_since(at) >= READ_AGAIN,
        }
    }

    fn wakes_in(&self, now: Instant) -> Duration {
        let after = if self.waiting { READ_LOST } else { READ_AGAIN };
        if self.now && !self.waiting {
            return Duration::ZERO;
        }
        self.asked_at
            .and_then(|at| at.checked_add(after))
            .map_or(Duration::ZERO, |due| due.saturating_duration_since(now))
    }

    fn ask(&mut self, now: Instant) {
        self.asked_at = Some(now);
        self.waiting = true;
        self.now = false;
    }
}

/// An app the person may open, as the catalogue lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Listed {
    id: String,
    title: String,
}

/// The shell of a page: what it read of the device, and what its views asked.
pub struct ShellPage {
    /// The workspace the page shows, by its id: where each workspace's desktop page is.
    workspace: String,
    origin: Rc<RefCell<LayoutOrigin>>,
    asked: Asked,
    /// The surface the page shows over its windows: empty at rest.
    here: String,
    layout_read: Read,
    apps_read: Read,
    styles_read: Read,
    /// The person's own workspace, where the device publishes the styles: `None` for a page
    /// whose person names no person key, which lists no styles.
    styles_workspace: Option<String>,
    /// The styles as last read, and the revision they were read at: the next read waits past it.
    styles: Option<Value>,
    styles_after: Option<String>,
    /// The layout's last answer, and the apps as last listed.
    layout: Option<Value>,
    apps: Option<Vec<Listed>>,
    /// The readings changed since the views were told.
    untold: bool,
    /// The layout reads in flight, and the layout's changes the page asked for.
    reading: Option<InvocationId>,
    changing: HashMap<InvocationId, &'static str>,
    /// The catalogue read in flight.
    listing: Option<u64>,
    /// The styles read in flight.
    styles_reading: Option<u64>,
    next_correlation: u64,
    queued: Vec<RemoteActionMessage>,
}

impl ShellPage {
    /// The shell of a page of `workspace` shown to `person`, writing the layout into `instance`
    /// from now on.
    pub fn new(workspace: &str, person: &str, instance: &mut Instance) -> Self {
        let origin = Rc::new(RefCell::new(LayoutOrigin::default()));
        instance.add_origin(Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin);
        Self {
            workspace: workspace.to_owned(),
            origin,
            asked: Rc::default(),
            here: String::new(),
            layout_read: Read::default(),
            apps_read: Read::default(),
            styles_read: Read::default(),
            styles_workspace: tilo_action_policy::roles::workspace_of_person(person),
            styles: None,
            styles_after: None,
            layout: None,
            apps: None,
            untold: true,
            reading: None,
            changing: HashMap::new(),
            listing: None,
            styles_reading: None,
            next_correlation: 1,
            queued: Vec::new(),
        }
    }

    /// Where the runtime sends the calls of the shell's views: what the page carries out is kept
    /// for it, and every other call goes to `others`.
    pub fn invoker(&self, others: impl Invoker + 'static) -> impl Invoker + 'static {
        Taking {
            asked: Rc::clone(&self.asked),
            others,
        }
    }

    /// How long until a read is due again.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        let now = ui_core::time::now();
        let soonest = self
            .layout_read
            .wakes_in(now)
            .min(self.apps_read.wakes_in(now))
            .min(self.styles_read.wakes_in(now));
        Some(std::time::Duration::from_millis(
            u64::try_from(soonest.as_millis()).unwrap_or(u64::MAX),
        ))
    }

    /// Take `event` when it answers one of the page's reads or changes. Whether it did.
    pub fn take(&mut self, event: &CollabEvent) -> bool {
        let CollabEvent::RemoteAction { message } = event else {
            return false;
        };
        let receipt = match message {
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            } if self.listing == Some(*correlation) => {
                self.listing = None;
                self.apps_read.waiting = false;
                match result {
                    Ok(page) => {
                        let listed = listed(page);
                        if self.apps.as_ref() != Some(&listed) {
                            self.apps = Some(listed);
                            self.untold = true;
                        }
                    }
                    Err(error) => {
                        tracing::debug!(?error, "the apps a page's shell lists were not read");
                    }
                }
                return true;
            }
            RemoteActionMessage::ResourceReading {
                correlation,
                result,
            } if self.styles_reading == Some(*correlation) => {
                self.styles_reading = None;
                self.styles_read.waiting = false;
                self.take_styles(result);
                return true;
            }
            RemoteActionMessage::Receipt(receipt) => receipt.clone(),
            RemoteActionMessage::Update {
                invocation,
                state: Some(state),
                ..
            } => InvocationReceipt {
                invocation: *invocation,
                status: state.receipt().status,
            },
            _ => return false,
        };
        if self.reading == Some(receipt.invocation) {
            if matches!(receipt.status, ReceiptStatus::Accepted) {
                return true;
            }
            self.reading = None;
            self.layout_read.waiting = false;
            match receipt.status {
                ReceiptStatus::Completed { output } => {
                    let read = output.value.unwrap_or_default();
                    if self.layout.as_ref() != Some(&read) {
                        self.layout = Some(read);
                        self.untold = true;
                    }
                }
                status => tracing::debug!(?status, "a page's shell did not read the layout"),
            }
            return true;
        }
        let Some(action) = self.changing.get(&receipt.invocation).copied() else {
            return false;
        };
        if matches!(receipt.status, ReceiptStatus::Accepted) {
            return true;
        }
        self.changing.remove(&receipt.invocation);
        if !matches!(receipt.status, ReceiptStatus::Completed { .. }) {
            tracing::warn!(action, status = ?receipt.status, "a page's change of the layout was refused");
        }
        // What it changed shows once the layout is read again.
        self.layout_read.now = true;
        true
    }

    /// Carry out what the views asked, read what is due, and tell `instance`'s views what moved.
    pub fn turn(&mut self, instance: &Instance) {
        let asked = std::mem::take(&mut *self.asked.borrow_mut());
        for invocation in &asked {
            self.carry_out(invocation);
        }
        let now = ui_core::time::now();
        if self.reading.is_none() && self.layout_read.due(now) {
            self.layout_read.ask(now);
            let request = invoke("read", Value::Null);
            self.reading = Some(request.invocation);
            self.queued.push(RemoteActionMessage::Invoke(request));
        }
        if self.listing.is_none() && self.apps_read.due(now) {
            self.apps_read.ask(now);
            let correlation = self.next_correlation;
            self.next_correlation = self.next_correlation.saturating_add(1);
            self.listing = Some(correlation);
            self.queued.push(RemoteActionMessage::CatalogueRequest {
                correlation,
                query: CatalogueQuery {
                    max_results: MOST_APPS,
                    ..CatalogueQuery::default()
                },
            });
        }
        if let Some(workspace) = &self.styles_workspace
            && self.styles_reading.is_none()
            && self.styles_read.due(now)
        {
            self.styles_read.ask(now);
            let correlation = self.next_correlation;
            self.next_correlation = self.next_correlation.saturating_add(1);
            self.styles_reading = Some(correlation);
            self.queued.push(RemoteActionMessage::ResourceRead {
                correlation,
                on_behalf_of: None,
                read: ResourceReadRequest {
                    workspace: workspace.clone(),
                    app: STYLES_APP.to_owned(),
                    record: String::new(),
                    joined: false,
                    wait_ms: self.styles_after.is_some().then_some(STYLES_WAIT_MS),
                    after: self.styles_after.clone(),
                    answer: ReadAnswer::Reading,
                },
            });
        }
        if self.untold {
            self.untold = false;
            if let Err(error) = self.tell(instance) {
                tracing::warn!(%error, "a page's shell was not told what it reads");
            }
        }
    }

    /// Send what is queued with `send`. A read the connection does not take is asked again when
    /// it is due; a change it does not take is not carried out.
    pub fn say(&mut self, mut send: impl FnMut(CollabRequest) -> bool) {
        for message in std::mem::take(&mut self.queued) {
            let invocation = match &message {
                RemoteActionMessage::Invoke(request) => Some(request.invocation),
                _ => None,
            };
            let correlation = match &message {
                RemoteActionMessage::CatalogueRequest { correlation, .. }
                | RemoteActionMessage::ResourceRead { correlation, .. } => Some(*correlation),
                _ => None,
            };
            if send(CollabRequest::RemoteAction {
                message,
                press: None,
            }) {
                continue;
            }
            if invocation.is_some() && invocation == self.reading {
                self.reading = None;
                self.layout_read.waiting = false;
            } else if let Some(invocation) = invocation
                && let Some(action) = self.changing.remove(&invocation)
            {
                tracing::warn!(action, "a page's change of the layout was not sent");
            }
            if correlation.is_some() && correlation == self.listing {
                self.listing = None;
                self.apps_read.waiting = false;
            }
            if correlation.is_some() && correlation == self.styles_reading {
                self.styles_reading = None;
                self.styles_read.waiting = false;
            }
        }
    }

    /// Carry out `invocation`, which the views asked of the session or the device.
    fn carry_out(&mut self, invocation: &Invocation) {
        match taken(invocation) {
            Some(Taken::Close(window)) => {
                let request = invoke("close", json!({ "window": window }));
                self.changing.insert(request.invocation, "close");
                self.queued.push(RemoteActionMessage::Invoke(request));
            }
            Some(Taken::Open(app)) => {
                // What the launcher opened stands on the layout, and the launcher closes over
                // it, as it does on a device.
                self.here.clear();
                self.untold = true;
                let request = invoke("place", json!({ "app": app }));
                self.changing.insert(request.invocation, "place");
                self.queued.push(RemoteActionMessage::Invoke(request));
            }
            Some(Taken::Surface(surface)) => {
                self.here = if self.here == surface {
                    String::new()
                } else {
                    surface
                };
                self.untold = true;
            }
            Some(Taken::Position(window, x, y)) => {
                let request = invoke("position", json!({ "window": window, "x": x, "y": y }));
                self.changing.insert(request.invocation, "position");
                self.queued.push(RemoteActionMessage::Invoke(request));
            }
            Some(Taken::Size(window, width, height)) => {
                let request = invoke(
                    "size",
                    json!({ "window": window, "width": width, "height": height }),
                );
                self.changing.insert(request.invocation, "size");
                self.queued.push(RemoteActionMessage::Invoke(request));
            }
            Some(Taken::Activate(window)) => {
                let request = invoke("activate", json!({ "window": window }));
                self.changing.insert(request.invocation, "activate");
                self.queued.push(RemoteActionMessage::Invoke(request));
            }
            // What the canvas sees of a hugging place is this device's view of it, which the
            // shared layout does not keep for a page: the window a page shows is its app's, and
            // the page lays its own view out.
            Some(Taken::Drawn) | None => {}
        }
    }

    /// Hand `instance`'s views the layout, the apps, the windows and the page's surface.
    fn tell(&self, instance: &Instance) -> Result<(), String> {
        let apps = self.apps.as_deref().unwrap_or_default();
        let reading = self.reading();
        let windows = windows_reading(&reading, self.layout.as_ref());
        self.origin.borrow_mut().read(reading);
        fill(
            instance,
            OVERLAYS,
            &json!({
                "here": if self.here.is_empty() { "none" } else { self.here.as_str() },
                "switcher_revealed": false,
                "switcher_scope": "workspace",
                "spotlight": "",
                "ask": "",
                "ask_detail": "",
                "ask_answer": "",
                "ask_open": false,
                "about": false,
                "emoji": false,
                "peek": Value::Null,
                "about_sheet": Value::Null,
            }),
        )?;
        fill(instance, "session:apps", &apps_reading(apps))?;
        if let Some(styles) = &self.styles {
            fill(instance, STYLES_APP, styles)?;
        }
        fill(instance, "session:windows", &windows)
    }

    /// Take in the styles a read answered: the reading and the revision it is at. A read that was
    /// refused or did not read is asked again when it is due, and the styles last read stand.
    fn take_styles(&mut self, result: &Result<String, tilo_actions::remote::ActionCallError>) {
        let readings = match result
            .as_ref()
            .map_err(ToString::to_string)
            .and_then(|text| serde_json::from_str::<Value>(text).map_err(|e| e.to_string()))
        {
            Ok(readings) => readings,
            Err(error) => {
                tracing::debug!(%error, "the styles a page's shell lists were not read");
                return;
            }
        };
        self.styles_after = readings
            .get("revision")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let Some(record) = readings.get("records").and_then(|records| records.get(0)) else {
            return;
        };
        let styles = if record["absent"].as_bool() == Some(true) {
            json!({ "offered": [] })
        } else {
            record["value"].clone()
        };
        if self.styles.as_ref() != Some(&styles) {
            self.styles = Some(styles);
            self.untold = true;
        }
    }

    /// The layout as the shell's views read it: each workspace leading to its own desktop page,
    /// the one shown first.
    fn reading(&self) -> LayoutReading {
        let Some(read) = &self.layout else {
            return LayoutReading::default();
        };
        let mut reading: LayoutReading = serde_json::from_value(read["layout"].clone())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "the layout a page's shell read is not one");
                LayoutReading::default()
            });
        let home = |workspace: &str| {
            tilo_server_protocol::page::PageAddress {
                workspace: workspace.to_owned(),
                app: tilo_server_protocol::page::DESKTOP.to_owned(),
                show: None,
            }
            .path()
        };
        for shown in &mut reading.workspaces {
            shown.home = home(&self.workspace);
        }
        for other in read["workspaces"].as_array().into_iter().flatten() {
            let key = other["key"].as_str().unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            reading.workspaces.push(LayoutWorkspaceReading {
                key: key.to_owned(),
                name: other["name"].as_str().unwrap_or_default().to_owned(),
                current: false,
                home: home(key),
            });
        }
        reading
    }
}

/// Hand `instance`'s views the reading of `published`, a resource a session publishes for its
/// shell.
fn fill(instance: &Instance, published: &str, reading: &Value) -> Result<(), String> {
    let ty = tilolang_tilo::platform_apps::platform_roots()
        .into_iter()
        .find(|(id, _)| *id == published)
        .map(|(_, ty)| ty)
        .ok_or_else(|| format!("`{published}` is no resource the platform publishes"))?;
    let target = tilolang_tilo::runtime::ResourceRef::parse_id(published, &ty)
        .ok_or_else(|| format!("`{published}` names no resource"))?;
    let host = instance
        .host()
        .ok_or_else(|| "the shell's host is not open".to_owned())?;
    // A root the host reads in its own workspace (its members) reads as it is there.
    if host.reads_in_workspace(&target) {
        return Ok(());
    }
    host.runtime()
        .set_reading(&target, &reading.to_string())
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A call of the layout's action `action` with `argument`, as the person at the page.
fn invoke(action: &str, argument: Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: LAYOUT_PROVIDER.to_owned(),
            address: ActionAddress::new(LAYOUT_RESOURCE, action),
        },
        argument: match argument {
            Value::Object(arguments) => InvokeArgument::Json(arguments),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The apps `page` lists, each by its id and its name: named as its own top resource is.
fn listed(page: &tilo_actions::remote::CataloguePage) -> Vec<Listed> {
    let mut apps: Vec<Listed> = Vec::new();
    for provider in &page.providers {
        let id = &provider.app_id;
        if id.is_empty() || apps.iter().any(|app| &app.id == id) {
            continue;
        }
        let title = page
            .rows
            .iter()
            .find(|row| &row.app_id == id && row.parent_resource.is_empty())
            .map(|row| row.resource.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| named(id));
        apps.push(Listed {
            id: id.clone(),
            title,
        });
    }
    apps.sort_by(|a, b| a.title.cmp(&b.title));
    apps
}

/// An app's name from its id, where nothing names it: the last part, capitalised.
fn named(id: &str) -> String {
    let last = id.rsplit_once('.').map_or(id, |(_, last)| last);
    let words = last.replace(['_', '-'], " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The apps `listed`, as `session:apps` lists them to a launcher. The catalogue names no category
/// for them.
fn apps_reading(listed: &[Listed]) -> Value {
    let installed: Vec<Value> = listed
        .iter()
        .map(|app| {
            json!({
                "key": app.id,
                "name": app.title,
                "icon": ui_core::icons::app_glyph(&app.id, None).name(),
                "category": "",
            })
        })
        .collect();
    json!({ "installed": installed })
}

/// The windows on `reading`'s layout as `session:windows` lists them to a dock, a launcher and a
/// switcher: every place, with where it stands and the app it runs, named as `read` names it.
fn windows_reading(reading: &LayoutReading, read: Option<&Value>) -> Value {
    let titles: BTreeMap<&str, &str> = read
        .and_then(|read| read["windows"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|window| Some((window["key"].as_str()?, window["app_title"].as_str()?)))
        .collect();
    let mut all = Vec::with_capacity(reading.places.len());
    for (at, place) in reading.places.iter().enumerate() {
        let strip = reading.strips.iter().find(|strip| strip.key == place.strip);
        let row = strip.and_then(|strip| reading.rows.iter().find(|row| row.key == strip.row));
        let workspace = strip.and_then(|strip| {
            reading
                .workspaces
                .iter()
                .find(|workspace| workspace.key == strip.workspace)
        });
        let title = if place.title.trim().is_empty() {
            place.name.clone()
        } else {
            place.title.clone()
        };
        // The share of its row the window keeps, as a fraction of the width the strip names.
        let width = strip.map_or(1000, |strip| strip.width);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a width in per-mille of a row is far below f64's exact range"
        )]
        let share = width as f64 / 1000.0;
        let at = i64::try_from(at.saturating_add(1)).unwrap_or(i64::MAX);
        all.push(json!({
            "key": place.key,
            "title": title,
            "origin_app": titles.get(place.key.as_str()).copied().unwrap_or_default(),
            "app_key": place.shows,
            "icon": ui_core::icons::app_glyph(&place.shows, None).name(),
            "strip": row.map_or("", |row| row.name.as_str()),
            "strip_key": place.strip,
            "workspace": workspace.map_or("", |workspace| workspace.name.as_str()),
            "workspace_key": strip.map_or("", |strip| strip.workspace.as_str()),
            "group_key": place.strip,
            "tab_index": 0,
            "share": share,
            "elsewhere": false,
            "device": "",
            "device_name": "",
            "place": at,
            "recency": at,
            "focused": strip.is_some_and(|strip| strip.active == place.key),
            "preview": "none",
            "not_responding": false,
        }));
    }
    json!({ "all": all })
}

/// What the page carries out of what a view asked.
enum Taken {
    /// Close the window with this key.
    Close(String),
    /// Open the app with this id.
    Open(String),
    /// Show this surface over the windows, or put it away when it shows.
    Surface(String),
    /// Stand the window with this key at this canvas point.
    Position(String, i64, i64),
    /// Give the window with this key this canvas size.
    Size(String, i64, i64),
    /// Bring the window with this key in front of its strip.
    Activate(String),
    /// What a place's canvas sees of its drawn height: this device's own, kept nowhere.
    Drawn,
}

/// What `invocation` asks the page to carry out; `None` for a call that goes through the dispatch.
fn taken(invocation: &Invocation) -> Option<Taken> {
    let Invocation::Action {
        app,
        name,
        target,
        args,
        ..
    } = invocation
    else {
        return None;
    };
    let text = |name: &str| {
        args.iter()
            .find(|(parameter, _)| parameter == name)
            .and_then(|(_, value)| match value {
                tilolang_tilo::runtime::Value::Text(text) => Some(text.clone()),
                _ => None,
            })
    };
    let place = |reference: &tilolang_tilo::runtime::ResourceRef| {
        reference
            .id_string()
            .strip_prefix(PLACE)
            .map(ToOwned::to_owned)
    };
    // The window a call acts on: the place it names, or the place it is bound to.
    let window = || {
        args.iter()
            .find_map(|(parameter, value)| match value {
                tilolang_tilo::runtime::Value::Ref(reference) if parameter == "place" => {
                    place(&reference.target)
                }
                _ => None,
            })
            .or_else(|| target.as_ref().and_then(place))
    };
    let number = |parameter: &str| {
        args.iter()
            .find(|(named, _)| named == parameter)
            .and_then(|(_, value)| match value {
                tilolang_tilo::runtime::Value::Int(number) => Some(*number),
                _ => None,
            })
    };
    if app.as_str() == SESSION_APP {
        match name.as_str() {
            "close" => return window().map(Taken::Close),
            // A drag let a place go here, and a drag of its edge sized it: the layout's own
            // actions carry them to the shared layout as the person's arranging step.
            "position" => {
                if let (Some(window), Some(x), Some(y)) = (window(), number("x"), number("y")) {
                    return Some(Taken::Position(window, x, y));
                }
            }
            "size" => {
                if let (Some(window), Some(width), Some(height)) =
                    (window(), number("width"), number("height"))
                {
                    return Some(Taken::Size(window, width, height));
                }
            }
            "activate" => return window().map(Taken::Activate),
            // What a canvas sees of a hugging place is this device's view, not a change of the
            // layout a page keeps: it is answered and dropped.
            "drawn" => return Some(Taken::Drawn),
            "open-app" => return text("app").map(Taken::Open),
            _ => {}
        }
    }
    if app.as_str() == DEVICE_APP
        && name == "show-surface"
        && target
            .as_ref()
            .is_some_and(|target| target.id_string() == OVERLAYS)
    {
        return Some(Taken::Surface(text("surface").unwrap_or_default()));
    }
    None
}

/// The shell's calls: what the page carries out is kept for it, and answered as done; every
/// other call goes to `others`.
struct Taking<I> {
    asked: Asked,
    others: I,
}

impl<I: Invoker> Invoker for Taking<I> {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        if taken(invocation).is_some() {
            self.asked.borrow_mut().push(invocation.clone());
            return Invoked::Done(tilolang_tilo::runtime::Value::Unit);
        }
        self.others.invoke(invocation)
    }
}
