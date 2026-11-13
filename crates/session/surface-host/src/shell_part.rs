//! A part of a tilolang shell, covering one output, drawn under the windows.
//!
//! A shell package's views run in a tilolang instance (`tilolang_tilo::instance`), exactly as
//! an app's do, and draw through the same UI host half (`tilo_ui_host::AppWindow`, here as
//! a [`AppWindow::part`]: the view as it is, no window shell). The toolkit lays that widget tree
//! out and the CPU rasterizes it, as [`WidgetHost`] does for every widget tree the session
//! shows; the compositor gets the buffer as a layer. Nothing here renders on its own.
//!
//! What the views read of the session comes in as resources whose origin is the session
//! (`type Desk (origin: session.desk)`): [`SessionOrigin`] writes what the session publishes on
//! its door (the `session:desk` resource's reading) into them. The views call the session's
//! actions as an app's (`use so.tilo.session as session`, then `session.show_surface(surface:
//! "launcher")`), each an invocation on its door ([`PlaceInvoker`]). A shell view never
//! reaches the session another way.
//!
//! The part works only when something changed: a press, a reading of the session's that moved,
//! or a door answer. Otherwise a frame asks nothing of the instance, and the toolkit draws
//! nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_session_core::surface::{CursorIcon, HostKey, KeyClaim};
use tilo_session_core::surface::{HostIme, HostLayer, HostTextInput, SurfaceHost};

use tilo_render_protocol::patch::local::pair;
use tilo_ui_host::AppWindow;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::platform_apps::platform_packages;
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::{Delivery, NameHost, Principal, ResourceRef, Value};
use tilolang_tilo::tilolang::Compiled;

use crate::{Placement, WidgetHost};

/// What a shell reads of the session and the names the session publishes it by: every host's,
/// so the shell's views and the session name one desk.
pub use tilo_session_core::shell::{
    ARRANGEMENT_RESOURCE, AgentReading, AppReading, ArrangementPlaceReading,
    ArrangementQueryReading, ArrangementReading, ArrangementRefReading, ArrangementRowReading,
    ArrangementStripReading, ArrangementWorkspaceReading, Ask, AskReading, CHROME_RESOURCE,
    DESK_RESOURCE, DeskReading, DeskStyleReading, NoticeActionReading, NoticeReading, ORIGIN,
    PersonReading, RoomReading, SHELL_APP, SHELL_RECORD, ScreenEdges, TimerReading, TrayReading,
    WindowReading, WindowRect, WorkspaceReading, reach_pad, screen_edges,
};

mod host;
mod settings;

/// Every file of the shell's package (`apps/tl/shell`) the session runs, by the name the
/// package knows it by: a package is a directory of `.tl` files, in path order (the package's
/// compiler reads them so), and each new one is listed here to ship. Its scenarios
/// (`scenarios.tl`), which stage the shell over other apps' records for its shots and tests,
/// are fixtures: the session never runs them, so it compiles the shell without them and needs
/// no other app's package to do so.
const SHELL_FILES: &[(&str, &str)] = &[
    ("app.tl", include_str!("../../../../apps/tl/shell/app.tl")),
    (
        "arrangement.tl",
        include_str!("../../../../apps/tl/shell/arrangement.tl"),
    ),
    (
        "canvas.tl",
        include_str!("../../../../apps/tl/shell/canvas.tl"),
    ),
    (
        "chrome.tl",
        include_str!("../../../../apps/tl/shell/chrome.tl"),
    ),
    ("desk.tl", include_str!("../../../../apps/tl/shell/desk.tl")),
    (
        "docks.tl",
        include_str!("../../../../apps/tl/shell/docks.tl"),
    ),
    (
        "launcher.tl",
        include_str!("../../../../apps/tl/shell/launcher.tl"),
    ),
    (
        "layout.tl",
        include_str!("../../../../apps/tl/shell/layout.tl"),
    ),
    (
        "notices.tl",
        include_str!("../../../../apps/tl/shell/notices.tl"),
    ),
    (
        "overview.tl",
        include_str!("../../../../apps/tl/shell/overview.tl"),
    ),
    (
        "parts.tl",
        include_str!("../../../../apps/tl/shell/parts.tl"),
    ),
    (
        "people.tl",
        include_str!("../../../../apps/tl/shell/people.tl"),
    ),
    (
        "presets.tl",
        include_str!("../../../../apps/tl/shell/presets.tl"),
    ),
    (
        "quick.tl",
        include_str!("../../../../apps/tl/shell/quick.tl"),
    ),
    (
        "rooms.tl",
        include_str!("../../../../apps/tl/shell/rooms.tl"),
    ),
    (
        "session.tl",
        include_str!("../../../../apps/tl/shell/session.tl"),
    ),
    (
        "strips.tl",
        include_str!("../../../../apps/tl/shell/strips.tl"),
    ),
    (
        "style_settings.tl",
        include_str!("../../../../apps/tl/shell/style_settings.tl"),
    ),
    (
        "switcher.tl",
        include_str!("../../../../apps/tl/shell/switcher.tl"),
    ),
    (
        "traditional.tl",
        include_str!("../../../../apps/tl/shell/traditional.tl"),
    ),
];
/// The shell package's ids lock.
const SHELL_IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");

/// The package id of the session's description.
pub const SESSION_APP: &str = tilolang_tilo::platform_apps::SESSION_APP;

/// The session's package: its resources' types and actions, which the shell's views call as
/// any package's (`session.show_surface(surface: "launcher")`).
///
/// # Errors
/// The description does not read.
pub fn session_package() -> Result<tilolang_tilo::tilolang::PackageMeta, String> {
    tilolang_tilo::platform_apps::session_package()
}

/// The resource each of the session's loose actions is published on, by the action's id.
fn session_addresses() -> BTreeMap<String, String> {
    tilolang_tilo::platform_apps::session_addresses().unwrap_or_else(|error| {
        tracing::error!(%error, "the session's description gives no addresses");
        BTreeMap::new()
    })
}

/// The session's loose actions as its description declares them, by id.
fn session_verbs() -> BTreeMap<String, ui::tilo_actions::surface::VerbSurface> {
    tilolang_tilo::platform_apps::session_verbs().unwrap_or_else(|error| {
        tracing::error!(%error, "the session's description does not read");
        BTreeMap::new()
    })
}

/// The shell package's ids lock, as it ships.
#[must_use]
pub const fn shell_ids() -> &'static str {
    SHELL_IDS
}

/// Every file of the shell package this build ships, by the name the package knows it by
/// (`app.tl`, `strips.tl`, …) and its source: the same copy [`compile_shell`] compiles, so
/// "make it mine" copies exactly the style a person is looking at (`shell.md` §6). Its
/// scenarios (`scenarios.tl`) are not here: they are fixtures, never part of the package a
/// session runs.
#[must_use]
pub const fn shell_sources() -> &'static [(&'static str, &'static str)] {
    SHELL_FILES
}

/// The shell package's own source-locale catalogue (`apps/tl/shell/locales/en-US.ftl`), as it
/// ships: what a copy of the shell carries so its own words travel with it, as an installed
/// app's do.
#[must_use]
pub const fn shell_catalogue() -> &'static str {
    include_str!("../../../../apps/tl/shell/locales/en-US.ftl")
}

/// The desk the session writes its reading into: the `desk` view over the `Desk` root, the
/// bar and the surface under it. The shell package has two roots (`Desk` and its `Arrangement`),
/// so each names its own; the instance is never "the first root's default view".
#[must_use]
pub fn desk_layer() -> RootView {
    RootView {
        root: Some("Desk".to_owned()),
        view: Some("desk".to_owned()),
    }
}

/// The shell's whole screen: the `desk_view` view over the person's arrangement (the package's
/// `arrangement` root), which stands the desk's bar, dock and surfaces around the places as the
/// preset the person picked lays them out. Each place an app's window shows in is a well, and
/// the window stands where that well does.
#[must_use]
pub fn screen_layer() -> RootView {
    RootView {
        root: Some("arrangement".to_owned()),
        view: Some("desk_view".to_owned()),
    }
}

/// The shell package, as the session runs it: the installed `so.tilo.shell` when the person's
/// app store holds one, and this build's copy (`compile_shell`) when it does not
/// (shell.md §6, "Where styles come from").
///
/// An installed shell is read the way every tilolang app is ([`seeded_apps`]'s path): its
/// payload from the store row, its module and the modules it uses, each of them compiled by
/// `tilolang build`. The store is the session's own (`$TILO_APPS_DATA_DIR`, else the person's
/// `$XDG_DATA_HOME/tilo-apps`), so a development session installs it as it installs the apps.
///
/// # Errors
/// The installed shell's payload does not read. A store that holds no shell is not an error:
/// the fallback answers.
pub fn installed_shell() -> Result<Option<Vec<Compiled>>, String> {
    let Ok(catalog) = package_store::Catalog::from_env() else {
        return Ok(None);
    };
    let found =
        app_catalogue::apps::read_one(&catalog, SHELL_APP).map_err(|error| error.to_string())?;
    let Some(app) = found else {
        return Ok(None);
    };
    let bundle = tilolang_tilo::bundle::Bundle::read(&app.payload_root)
        .map_err(|error| format!("{SHELL_APP}: {error}"))?;
    // The packages it uses come first, as a bundle holds them, the shell last: the runtime
    // loads each before the one that uses it.
    let mut out: Vec<Compiled> = Vec::new();
    for import in &bundle.imports {
        out.push(compiled_of(&import.wasm, &import.meta, ""));
    }
    out.push(compiled_of(&bundle.wasm, &bundle.meta, &bundle.ids));
    Ok(Some(out))
}

/// A compiled package made of the module `wasm`, the metadata `meta` and the ids lock `ids`,
/// as an installed payload holds them: the shape a shell's instance loads.
fn compiled_of(wasm: &[u8], meta: &tilolang_tilo::tilolang::PackageMeta, ids: &str) -> Compiled {
    Compiled {
        wasm: wasm.to_vec(),
        meta: meta.clone(),
        ids: tilolang_tilo::tilolang::IdsLock::parse(ids).unwrap_or_default(),
    }
}

/// The shell package, compiled: what a shell's instance loads, over the session's and the
/// platform's descriptions. No other app's package is the shell's: the package of each app a
/// place shows comes from the session when a place shows it ([`SessionLink::publish_packages`]).
/// `Err` when it does not compile, which every build of the shell is checked against, so a
/// test that fails to compile here fails loudly.
///
/// This is the fallback shell, for a device whose app store holds no `so.tilo.shell` (or one
/// that does not load): [`installed_shell`] answers with the installed copy first.
///
/// # Errors
/// The package's diagnostics, rendered with their file names.
pub fn compile_shell() -> Result<Vec<Compiled>, String> {
    let session = session_package()?;
    let mut out: Vec<Compiled> = Vec::new();
    let lock = tilolang_tilo::tilolang::IdsLock::parse(SHELL_IDS).map_err(|e| e.to_string())?;
    let source = tilolang_tilo::tilolang::PackageSource::new(
        SHELL_FILES
            .iter()
            .map(|(path, text)| (format!("apps/tl/shell/{path}"), (*text).to_owned())),
    )
    // The shell's own words travel in its module, as an installed app's do.
    .with_locales([(
        "en-US".to_owned(),
        include_str!("../../../../apps/tl/shell/locales/en-US.ftl").to_owned(),
    )]);
    let mut imports: Vec<tilolang_tilo::tilolang::PackageMeta> = platform_packages()?
        .into_iter()
        .filter(|meta| meta.app != SESSION_APP)
        .collect();
    imports.insert(0, session);
    let shell = tilolang_tilo::tilolang::compile_package_source(&source, &lock, &imports).map_err(
        |diagnostics| {
            diagnostics
                .iter()
                .map(|d| d.render(d.file.as_deref().unwrap_or("apps/tl/shell/app.tl")))
                .collect::<Vec<_>>()
                .join("\n")
        },
    )?;
    out.push(shell);
    Ok(out)
}
/// How often the instance and its window may answer each other in one frame before the frame
/// is drawn as it is: a press, the action it ran, the change it committed, the effect settled.
const EXCHANGES: usize = 8;

/// Between the session and the shell's origin: the desk's reading the session last published,
/// what the shell asked of the door, and the door's answers. The session serves it every turn
/// of its loop ([`SessionLink::serve`]); the origin reads and writes the rest.
#[derive(Default)]
pub struct SessionLink {
    reading: RefCell<Option<DeskReading>>,
    /// A reading arrived the origin has not written yet.
    fresh: Cell<bool>,
    /// The person's arrangement the session last published, and whether the origin has taken
    /// it in.
    arrangement: RefCell<Option<ArrangementReading>>,
    arrangement_fresh: Cell<bool>,
    /// The readings of the resources the places name, by their session id: each with its type
    /// as the shell's runtime names another app's (`<app>#<Type>`) and its JSON.
    readings: RefCell<BTreeMap<String, (String, serde_json::Value)>>,
    /// The resources whose readings arrived and the origin has not handed the runtime yet.
    readings_fresh: RefCell<BTreeSet<String>>,
    /// The packages of the apps whose resources the places show, by app id: each its compiled
    /// module, which the shell's runtime loads to draw those resources through their app's
    /// own views, and unloads once no place shows that app.
    packages: RefCell<BTreeMap<String, std::sync::Arc<[u8]>>>,
    packages_fresh: Cell<bool>,
    /// The verbs the session's resources publish that no loaded package declares, by resource
    /// and verb ([`tilolang_tilo::runtime::published`]): what the shell's runtime sends to the app
    /// publishing them. `None` until the session publishes one.
    published: RefCell<Option<tilolang_tilo::runtime::published::Published>>,
    published_fresh: Cell<bool>,
    /// The resources a view composed in a place read before their readings came in, by id,
    /// each with its type: what the session publishes next ([`SessionLink::take_wanted`]).
    wanted: RefCell<BTreeMap<String, String>>,
    /// The apps whose package a view composed in a place read a resource of before it was
    /// loaded, by app id, each with the type it read: what the session publishes next
    /// ([`SessionLink::take_wanted_packages`]).
    wanted_packages: RefCell<BTreeMap<String, String>>,
    /// The views that could not be shown — a package that did not load, a view that failed —
    /// each an app, a view and why: what the session hears so it can say so where the person's
    /// own choice is the one that failed (`shell.md` §6, "Nothing can lock a person out").
    view_failures: RefCell<Vec<tilolang_tilo::runtime::foreign::ViewFailure>>,
    /// The apps whose view failure ended — its package now loads: what the session hears so a
    /// style the person chose comes back (`shell.md` §6).
    view_recoveries: RefCell<Vec<String>>,
    /// The view calls whose failure ended — the call ran again and succeeded, each an app and a
    /// view: what the session hears so a style the person chose comes back (`shell.md` §6).
    recovered_calls: RefCell<Vec<(String, String)>>,
    asked: RefCell<VecDeque<Ask>>,
    /// How many effects the shell handed the session, and how many the session ran.
    handed: Cell<u64>,
    served: Cell<u64>,
    /// What the door refused of what the shell asked, in its words: each refusal, kept for
    /// whoever asks ([`SessionLink::refused`]), never a quiet drop.
    refused: RefCell<Vec<String>>,
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

    /// The session published `reading`, the person's arrangement as it stands now: the shell
    /// writes it on its next frame when it differs from the last. Whether it did.
    pub fn publish_arrangement(&self, reading: ArrangementReading) -> bool {
        if self.arrangement.borrow().as_ref() == Some(&reading) {
            return false;
        }
        *self.arrangement.borrow_mut() = Some(reading);
        self.arrangement_fresh.set(true);
        true
    }

    /// Run what the shell asked through `invoke` (the session's door), keeping each refusal.
    /// Whether anything ran.
    pub fn serve(&self, mut invoke: impl FnMut(&Ask) -> Result<(), String>) -> bool {
        let asked: Vec<Ask> = self.asked.borrow_mut().drain(..).collect();
        self.served
            .set(self.served.get().wrapping_add(asked.len() as u64));
        for ask in &asked {
            let answer = invoke(ask);
            if let Err(why) = &answer {
                tracing::warn!(?ask, why, "the session refused what the shell asked");
                self.refused
                    .borrow_mut()
                    .push(format!("{} on {}: {why}", ask.action, ask.resource));
            }
        }
        !asked.is_empty()
    }

    /// What the door refused of what the shell asked since the session started, in its words.
    #[must_use]
    pub fn refused(&self) -> Vec<String> {
        self.refused.borrow().clone()
    }

    /// The shell asks `ask` of the session's door: run on the session's next turn
    /// ([`Self::serve`]).
    pub fn ask(&self, ask: Ask) {
        self.asked.borrow_mut().push_back(ask);
        self.handed.set(self.handed.get().wrapping_add(1));
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
        // Written here, not through [`Self::publish`]: that keeps the edges the reading before
        // it held, which is what a desk change must not overwrite, and these are the edges
        // themselves.
        *self.reading.borrow_mut() = Some(reading);
        self.fresh.set(true);
    }

    /// The surface the desk shows now (`none` at rest): what the part covers the outputs for.
    #[must_use]
    pub fn surface(&self) -> String {
        self.reading
            .borrow()
            .as_ref()
            .map(|reading| reading.here.clone())
            .unwrap_or_default()
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
    people: BTreeMap<String, Kept<PersonReading>>,
    windows: BTreeMap<String, Kept<WindowReading>>,
    agents: BTreeMap<String, Kept<AgentReading>>,
    rooms: BTreeMap<String, Kept<RoomReading>>,
    notices: BTreeMap<String, Kept<NoticeReading>>,
    notice_actions: BTreeMap<String, Kept<NoticeActionReading>>,
    styles: BTreeMap<String, Kept<DeskStyleReading>>,
    arrangement_workspaces: BTreeMap<String, Kept<ArrangementWorkspaceReading>>,
    arrangement_rows: BTreeMap<String, Kept<ArrangementRowReading>>,
    arrangement_strips: BTreeMap<String, Kept<ArrangementStripReading>>,
    arrangement_places: BTreeMap<String, Kept<ArrangementPlaceReading>>,
    arrangement_queries: BTreeMap<String, Kept<ArrangementQueryReading>>,
    arrangement_refs: BTreeMap<String, Kept<ArrangementRefReading>>,
    tray: BTreeMap<String, Kept<TrayReading>>,
    apps: BTreeMap<String, Kept<AppReading>>,
    /// The packages of other apps loaded into the shell's runtime, by app id: each the module
    /// the session handed for it.
    loaded: BTreeMap<String, std::sync::Arc<[u8]>>,
}

impl SessionOrigin {
    #[must_use]
    pub fn new(link: Rc<SessionLink>) -> Self {
        Self {
            loaded: BTreeMap::new(),
            link,
            written: None,
            workspaces: BTreeMap::new(),
            people: BTreeMap::new(),
            windows: BTreeMap::new(),
            agents: BTreeMap::new(),
            rooms: BTreeMap::new(),
            notices: BTreeMap::new(),
            notice_actions: BTreeMap::new(),
            styles: BTreeMap::new(),
            arrangement_workspaces: BTreeMap::new(),
            arrangement_rows: BTreeMap::new(),
            arrangement_strips: BTreeMap::new(),
            arrangement_places: BTreeMap::new(),
            arrangement_queries: BTreeMap::new(),
            arrangement_refs: BTreeMap::new(),
            tray: BTreeMap::new(),
            apps: BTreeMap::new(),
        }
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
            "quiet",
            Value::Bool(reading.quiet),
            reading.quiet != was.quiet,
        );
        set(
            "workspace",
            text(&reading.workspace),
            reading.workspace != was.workspace,
        );
        set("strip", text(&reading.strip), reading.strip != was.strip);
        // What the screen says of the edges a bar or dock that shows only sometimes follows.
        // They are the desk's, derived with the rest of it: the session makes them again every
        // start, so they are never the person's record and never need committing.
        set(
            "bar_reached",
            Value::Bool(reading.bar_reached),
            reading.bar_reached != was.bar_reached,
        );
        set(
            "bar_covered",
            Value::Bool(reading.bar_covered),
            reading.bar_covered != was.bar_covered,
        );
        set(
            "dock_reached",
            Value::Bool(reading.dock_reached),
            reading.dock_reached != was.dock_reached,
        );
        set(
            "dock_covered",
            Value::Bool(reading.dock_covered),
            reading.dock_covered != was.dock_covered,
        );
        set("here", text(&reading.here), reading.here != was.here);
        // The switcher's panel: whether it shows yet (a flick draws nothing), and which
        // windows its walk crosses.
        set(
            "switcher_revealed",
            Value::Bool(reading.switcher_revealed),
            reading.switcher_revealed != was.switcher_revealed,
        );
        set(
            "switcher_scope",
            text(&reading.switcher_scope),
            reading.switcher_scope != was.switcher_scope,
        );
        set(
            "spotlight",
            text(&reading.spotlight),
            reading.spotlight != was.spotlight,
        );
        set(
            "banner",
            text(&reading.banner),
            reading.banner != was.banner,
        );
        // The timer Clocks runs: its name and what it reads. Empty names mean no timer, so
        // the clock shows the time alone; paused and stopwatch say how to read it.
        let timer = reading.timer.clone().unwrap_or_default();
        set(
            "timer",
            text(&reading.timer_name()),
            reading.timer != was.timer,
        );
        set(
            "timer_left",
            text(&reading.timer_left()),
            reading.timer != was.timer,
        );
        set(
            "timer_paused",
            Value::Bool(timer.paused),
            reading.timer != was.timer,
        );
        set(
            "timer_stopwatch",
            Value::Bool(timer.stopwatch),
            reading.timer != was.timer,
        );
        // The ask on the person's avatar: its words, and whether it stands open.
        let ask = reading.ask.clone().unwrap_or_default();
        set("ask", text(&ask.question), reading.ask != was.ask);
        set("ask_detail", text(&ask.detail), reading.ask != was.ask);
        set("ask_answer", text(&ask.answer), reading.ask != was.ask);
        set("ask_open", Value::Bool(ask.open), reading.ask != was.ask);
        let mut writes = Vec::new();
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: root.clone(),
                fields,
            });
        }
        // The running applications' tray items: each's key and the app's own words for it.
        let tray = reading.tray.clone();
        if let (Ok(ty), Some(collection)) = (kind("tray_item"), field(desk, "tray")) {
            writes.extend(sync_items(
                &mut self.tray,
                &tray,
                root,
                &collection,
                ty,
                |item| item.key.clone(),
                |item| {
                    vec![
                        ("key", text(&item.key)),
                        ("title", text(&item.title)),
                        ("menu", Value::Bool(item.menu)),
                    ]
                },
            ));
        }
        // The installed applications: what the launcher lists and opens.
        if let (Ok(ty), Some(collection)) = (kind("application"), field(desk, "apps")) {
            writes.extend(sync_items(
                &mut self.apps,
                &reading.apps,
                root,
                &collection,
                ty,
                |app| app.key.clone(),
                |app| {
                    vec![
                        ("key", text(&app.key)),
                        ("name", text(&app.name)),
                        ("icon", text(&app.icon)),
                        ("category", text(&app.category)),
                        ("description", text(&app.description)),
                    ]
                },
            ));
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
        if let (Ok(ty), Some(collection)) = (kind("window"), field(desk, "windows")) {
            writes.extend(sync_items(
                &mut self.windows,
                &reading.windows,
                root,
                &collection,
                ty,
                |w| w.key.clone(),
                |w| {
                    vec![
                        ("key", text(&w.key)),
                        ("title", text(&w.title)),
                        ("origin_app", text(&w.origin_app)),
                        ("app_key", text(&w.app_key)),
                        ("icon", text(&w.icon)),
                        ("strip", text(&w.strip)),
                        ("strip_key", text(&w.strip_key)),
                        ("workspace", text(&w.workspace)),
                        ("workspace_key", text(&w.workspace_key)),
                        ("group_key", text(&w.group_key)),
                        ("tab_index", Value::Int(w.tab_index)),
                        ("share", Value::Float(w.share)),
                        ("local", Value::Bool(w.local)),
                        ("device", text(&w.device)),
                        ("device_name", text(&w.device_name)),
                        ("place", Value::Int(w.place)),
                        ("recency", Value::Int(w.recency)),
                        ("focused", Value::Bool(w.focused)),
                        ("preview", text(&w.preview)),
                        ("not_responding", Value::Bool(w.not_responding)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("agent"), field(desk, "agents")) {
            writes.extend(sync_items(
                &mut self.agents,
                &reading.agents,
                root,
                &collection,
                ty,
                |a| a.key.clone(),
                |a| {
                    vec![
                        ("key", text(&a.key)),
                        ("name", text(&a.name)),
                        ("state", text(&a.state)),
                        ("doing", text(&a.doing)),
                        ("mark", text(&a.mark)),
                        ("unseen", Value::Int(a.unseen)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("room"), field(desk, "rooms")) {
            writes.extend(sync_items(
                &mut self.rooms,
                &reading.rooms,
                root,
                &collection,
                ty,
                |r| r.key.clone(),
                |r| {
                    vec![
                        ("key", text(&r.key)),
                        ("name", text(&r.name)),
                        ("current", Value::Bool(r.current)),
                        ("members", text(&r.members)),
                        ("present", Value::Int(r.present)),
                        ("activity", text(&r.activity)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("notice"), field(desk, "notices")) {
            writes.extend(sync_items(
                &mut self.notices,
                &reading.notices,
                root,
                &collection,
                ty,
                |n| n.key.clone(),
                |n| {
                    vec![
                        ("key", text(&n.key)),
                        ("title", text(&n.title)),
                        ("body", text(&n.body)),
                        ("origin_app", text(&n.origin_app)),
                        ("kind", text(&n.kind)),
                        ("seen", Value::Bool(n.seen)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("notice_action"), field(desk, "notice_actions")) {
            writes.extend(sync_items(
                &mut self.notice_actions,
                &reading.notice_actions,
                root,
                &collection,
                ty,
                |a| format!("{}:{}", a.notice, a.key),
                |a| {
                    vec![
                        ("notice", text(&a.notice)),
                        ("key", text(&a.key)),
                        ("label", text(&a.label)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("desk_style"), field(desk, "styles")) {
            writes.extend(sync_items(
                &mut self.styles,
                &reading.styles,
                root,
                &collection,
                ty,
                |s| s.style.clone(),
                |s| {
                    vec![
                        ("key", text(&s.style)),
                        ("style", text(&s.style)),
                        ("package", text(&s.package)),
                        ("name", text(&s.name)),
                        ("line", text(&s.line)),
                        ("loads", Value::Bool(s.loads)),
                        ("reason", text(&s.reason)),
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

/// The field `name` as a text value.
fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

impl SessionOrigin {
    /// Write the person's arrangement into the shell's `Arrangement` root, when the session
    /// published one the origin has not written yet. Whether anything was written.
    fn poll_arrangement(&mut self, cx: &OriginCx<'_>) -> bool {
        if !self.link.arrangement_fresh.replace(false) {
            return false;
        }
        let Some(reading) = self.link.arrangement.borrow().clone() else {
            return false;
        };
        let Some(types) = cx.host.find_in_packages(|package| {
            (package.meta.app == cx.app).then(|| origin_types(&package.meta, ORIGIN))
        }) else {
            tracing::error!(app = cx.app, "the shell's package is not loaded");
            return false;
        };
        // The root the `Arrangement` type is: `root-<type id>`, made the first time it is
        // wanted, as `root arrangement` reads it.
        let Some(kind) = types
            .iter()
            .find(|ty| ty.kind == format!("{ORIGIN}.arrangement"))
        else {
            // A shell without an arrangement view keeps its bar alone: nothing to write.
            return false;
        };
        let root = if cx.host.store().contains(&format!("root-{}", kind.ty)) {
            ResourceRef::root(format!("root-{}", kind.ty), kind.ty.clone())
        } else {
            match cx.host.runtime().create_root(&kind.ty) {
                Ok(root) => root,
                Err(error) => {
                    tracing::error!(%error, "the shell has no arrangement to write");
                    return false;
                }
            }
        };
        let (writes, inserted) = self.arrangement_writes(&reading, &root, &types);
        match cx
            .host
            .runtime()
            .apply_origin(ORIGIN, writes, Some(cx.now.clone()))
        {
            Ok(made) => {
                self.keep_inserted_arrangement(&reading, &types, inserted, made);
                true
            }
            Err(error) => {
                tracing::error!(%error, "the person's arrangement could not be written");
                false
            }
        }
    }

    /// The writes that bring the shell's arrangement root from what was written to `reading`,
    /// and the items they insert, by type and key.
    fn arrangement_writes(
        &mut self,
        reading: &ArrangementReading,
        root: &ResourceRef,
        types: &[OriginType],
    ) -> (Vec<OriginWrite>, Vec<(String, String)>) {
        let find = |name: &str| {
            types
                .iter()
                .find(|ty| ty.kind == format!("{ORIGIN}.{name}"))
        };
        let mut writes = Vec::new();
        let Some(ty) = find("arrangement") else {
            return (writes, Vec::new());
        };
        if let (Some(field), Some(item)) = (ty.field("workspaces"), find("arrangement_workspace")) {
            writes.extend(sync_items(
                &mut self.arrangement_workspaces,
                &reading.workspaces,
                root,
                field,
                item,
                |w| w.key.clone(),
                readings::workspace_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("rows"), find("arrangement_row")) {
            writes.extend(sync_items(
                &mut self.arrangement_rows,
                &reading.rows,
                root,
                field,
                item,
                |r| r.key.clone(),
                readings::row_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("strips"), find("arrangement_strip")) {
            writes.extend(sync_items(
                &mut self.arrangement_strips,
                &reading.strips,
                root,
                field,
                item,
                |s| s.key.clone(),
                readings::strip_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("places"), find("arrangement_place")) {
            writes.extend(sync_items(
                &mut self.arrangement_places,
                &reading.places,
                root,
                field,
                item,
                |p| p.key.clone(),
                readings::place_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("queries"), find("arrangement_query")) {
            // The refs each query found live in their own collection, keyed by their `key`
            // (as `<query>/<number>`), so a query's rows are the refs that name it.
            let query = |q: &ArrangementQueryReading| q.key.clone();
            writes.extend(sync_items(
                &mut self.arrangement_queries,
                &reading.queries,
                root,
                field,
                item,
                query,
                readings::query_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("refs"), find("arrangement_ref")) {
            let refs: Vec<ArrangementRefReading> = reading
                .queries
                .iter()
                .flat_map(|query| query.matches.iter().cloned())
                .collect();
            writes.extend(sync_items(
                &mut self.arrangement_refs,
                &refs,
                root,
                field,
                item,
                |r| r.key.clone(),
                readings::ref_fields,
            ));
        }
        // The keys of the items the writes insert, in their order: what the origin must find
        // where they were made.
        let inserted = writes
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
                        });
                    Some((ty.clone(), key.unwrap_or_default()))
                }
                _ => None,
            })
            .collect();
        (writes, inserted)
    }

    /// Keep where the arrangement items a write inserted were made, by type and key.
    fn keep_inserted_arrangement(
        &mut self,
        reading: &ArrangementReading,
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
            if is("arrangement_workspace")
                && let Some(was) = reading.workspaces.iter().find(|w| w.key == key)
            {
                self.arrangement_workspaces.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("arrangement_row")
                && let Some(was) = reading.rows.iter().find(|r| r.key == key)
            {
                self.arrangement_rows.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("arrangement_strip")
                && let Some(was) = reading.strips.iter().find(|s| s.key == key)
            {
                self.arrangement_strips.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("arrangement_place")
                && let Some(was) = reading.places.iter().find(|p| p.key == key)
            {
                self.arrangement_places.insert(
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
            } else if is("window")
                && let Some(was) = reading.windows.iter().find(|w| w.key == key)
            {
                self.windows.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("agent")
                && let Some(was) = reading.agents.iter().find(|a| a.key == key)
            {
                self.agents.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("room")
                && let Some(was) = reading.rooms.iter().find(|r| r.key == key)
            {
                self.rooms.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("notice")
                && let Some(was) = reading.notices.iter().find(|n| n.key == key)
            {
                self.notices.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("notice_action")
                && let Some(was) = reading
                    .notice_actions
                    .iter()
                    .find(|a| format!("{}:{}", a.notice, a.key) == key)
            {
                self.notice_actions.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("desk_style")
                && let Some(was) = reading.styles.iter().find(|s| s.style == key)
            {
                self.styles.insert(
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

    /// The shell calls the session's actions as an app's (`session.show_surface(...)`), each an
    /// invocation on its door ([`PlaceInvoker`]); no outside effect is the session's.
    fn serves(&self, _capability: &str) -> bool {
        false
    }

    fn due(&self) -> bool {
        self.link.fresh.get()
            || self.link.arrangement_fresh.get()
            || self.link.packages_fresh.get()
            || self.link.published_fresh.get()
            || !self.link.readings_fresh.borrow().is_empty()
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let mut polled = Polled {
            wrote: false,
            settle: false,
        };
        let wrote_arrangement = self.poll_arrangement(cx);
        polled.wrote |= wrote_arrangement;
        // The packages the places need first: a reading of a resource is read as its app's
        // package declares it.
        polled.wrote |= self.poll_packages(cx);
        polled.wrote |= self.poll_readings(cx);
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
        let root = match cx.host.root_of_layer(cx.app, &desk_layer()) {
            Ok(root) => root,
            Err(error) => {
                tracing::error!(%error, "the shell has no desk to write the session into");
                return polled;
            }
        };
        let writes = match self.writes(&reading, &root, &types) {
            // The desk's own change is applied below, in the origin's own transaction.
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

    fn deliver(&mut self, capability: &str, _payload: &Json) -> Delivery {
        Delivery::Rejected(format!(
            "the session takes no outside effect `{capability}`"
        ))
    }
}

/// What a shell's instance asks of the session's process.
struct ShellPlatform {
    packages: Rc<Vec<Compiled>>,
    waker: Waker,
    /// Where what the composed views change of other apps' resources goes.
    link: Rc<SessionLink>,
}

impl Platform for ShellPlatform {
    fn load(&self, host: &Host) -> u64 {
        // The device's and Settings' packages first: the shell's views read their resources
        // (`device.sound`) from the readings the session publishes, and pressing their actions
        // asks the session, as a place's composed view does.
        match platform_packages() {
            Ok(packages) => {
                for meta in packages {
                    // The session's package is loaded below, from the copy the shell was built
                    // against, which is the only description whose action ids its code calls.
                    if meta.app == SESSION_APP {
                        continue;
                    }
                    let app = meta.app.clone();
                    if let Err(error) = host.runtime().load_foreign(meta) {
                        tracing::error!(app, %error, "a platform package did not load");
                    }
                }
            }
            Err(why) => tracing::error!(why, "the platform's packages do not read"),
        }
        // Other apps' packages come as places show their resources ([`SessionOrigin`]): their
        // records are the ones their apps keep, read through the session
        // ([`SessionLink::follow_places`]), and what their views change goes to those records'
        // doors ([`PlaceInvoker`]). The shell's record holds the shell's own records only.
        let Some(shell) = self.packages.last() else {
            return 0;
        };
        // The session's own package is a description: its actions run at the session's door.
        // It is the one the shell was built against — the bundle's own import, whose action ids
        // are the ids the shell's code calls (`so.tilo.session#tilo-menu`). A package read again
        // from the source describes the same resources under the same names, but names its
        // actions as the surface does (`tilo-menu`), which is not what a compiled call names:
        // the runtime would find no action for it and the view would fail. The source's
        // description stands in only for a shell compiled against it, which has no bundle.
        let built_against = self
            .packages
            .iter()
            .find(|package| package.meta.app == SESSION_APP)
            .map(|package| package.meta.clone());
        match built_against.map_or_else(session_package, Ok) {
            Ok(session) => {
                if let Err(error) = host.runtime().load_foreign(session) {
                    tracing::error!(%error, "the session's package did not load");
                }
            }
            Err(error) => tracing::error!(%error, "the session's description does not read"),
        }
        if let Err(error) = host.load_compiled(shell) {
            tracing::error!(app = shell.meta.app, %error, "the shell package did not load");
        }
        host.runtime().set_invoker(PlaceInvoker {
            link: Rc::clone(&self.link),
            session: session_addresses(),
            verbs: session_verbs(),
        });
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_ui_host::i18n::word(id, args, id)
    }

    /// The time of day as the person reads a clock: the shell's views run in the session, which
    /// holds their settings.
    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
    }

    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        if window {
            tilo_ui_host::i18n::add_catalogues(catalogues);
        } else {
            tilo_ui_host::i18n::add_shared_catalogues(catalogues);
        }
    }

    fn words_revision(&self) -> u64 {
        tilo_ui_host::i18n::revision()
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

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    /// The day the person's region starts its week on, as they chose it in Settings: the shell's
    /// views run in the session, which holds their formats, so a day's `start_of_week()` goes
    /// back to the day their desk's calendar starts a week on.
    fn week_start(&self) -> i64 {
        i64::from(ui::format::first_weekday().num_days_from_monday())
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
        } else if who.1.trim().is_empty() {
            // Named by what the actor is: an app by its own name in the person's language, an
            // agent by its name, a person by theirs. Never a principal id.
            ui::history_source::participant_name(&who.0)
        } else {
            who.1.clone()
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
    use tilo_action_policy::{
        Answer, Grant, KindSet, Standing, Subject, TreePath, WorkspacePolicy,
    };
    let mut policy = WorkspacePolicy::new(SHELL_ROOM, person);
    policy.grants.push(Grant::new(
        format!("shell:{app}:session"),
        Subject::app(app),
        TreePath::workspace(SHELL_ROOM).app(app),
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
    /// The shell's record, shared by every part of one shell: every display draws the same
    /// record, so a change any part commits is the whole shell's.
    record: Rc<RefCell<tilo_resource::Doc>>,
    origin: Rc<RefCell<SessionOrigin>>,
    /// Input reached the part since its last turn.
    input: bool,
    /// The last turn changed something: the next looks again, for what that started.
    again: bool,
    work: ShellWork,
    /// What the shell refused, in its words.
    refusals: Vec<String>,
    /// The output this part stands on: its connector name, and its logical rectangle
    /// `(x, y, w, h)`. `None` before the outputs are told, when it spans their bounding box.
    output: Option<(String, (i32, i32, u32, u32))>,
    /// The scale the output it stands on draws at: what the part paints at so it is as crisp
    /// as the display allows, rather than drawn at one scale and stretched over another.
    output_scale: f64,
    /// The display the part stood on, which went (unplugged, turned off): while it is gone the
    /// part stands nowhere, draws nothing, takes no pointer and offers nothing on the door,
    /// until it is told a display again.
    gone: Option<String>,
    /// The outputs a fullscreen window covers, by connector name: the part on one of them
    /// steps aside, as the desktop's own bar does there, and leaves the display to the window.
    fullscreen: Vec<String>,
    /// A menu the part opened, or a surface of the desk's (its overview, the switcher, the
    /// launcher, the notifications), is up: the part stands over its output even where a
    /// fullscreen window owns it, so what the person opened shows.
    cover: bool,
    /// The shell's record's heads the instance was last brought up to date with: a commit any
    /// part of the shell made reaches this one by them, so every display shows one shell.
    seen_heads: Vec<String>,
    /// The previews the part's views name ([`ui::widget::WindowPreview`]), and which of the
    /// compositor's windows each is: the part hands the compositor every window's live picture
    /// where its view would draw it, and none for a window `windows` leaves out (one that may
    /// not be pictured).
    previews: Option<(ui::widget::Previews, WindowLookup)>,
    /// An instant the session has something of its own to draw at (the switcher's panel
    /// coming up while Alt is held): the part wakes then rather than sleeping over it.
    deadline: Option<std::time::Instant>,
    /// The content its places' pictures are read from: the session's document media, as the
    /// workspace in front's places stand in it. The part draws in the session's own process,
    /// which the launcher handed no capability of its own, so what an app's connector kept for
    /// the records a place shows is read from here (`ui::pictures::lend_content`).
    content: Option<ui::content::ContentStore>,
    /// The workspace the store was opened for, as the session names it: a place of another
    /// workspace reads its pictures from that one's, and the session lends the store of the
    /// workspace in front each pass.
    content_workspace: String,
}

/// Which of the compositor's windows a view's window key names, if it may be pictured.
type WindowLookup = Rc<dyn Fn(&str) -> Option<u64>>;

/// A shell part's identity: the package it runs, the person it is for, and the record the
/// shell's parts share.
pub struct ShellIdentity<'a> {
    /// The app id of the compiled shell package.
    pub app: &'a str,
    /// The packages the instance runs, the shell last.
    pub packages: Vec<Compiled>,
    /// The person the shell is arranged by.
    pub person: &'a str,
    /// The link the session publishes its desk on and the shell asks through.
    pub link: Rc<SessionLink>,
    /// The shell's record, shared by every part of the shell.
    pub record: Rc<RefCell<tilo_resource::Doc>>,
    /// The first part of a shell makes its record and seeds it; every other part finds it
    /// made, so one shell has one record and one seed, however many displays draw it.
    pub seed: bool,
}

impl ShellPart {
    /// The root view of `identity.app` (the last of its packages, which come in load order) for
    /// its person, across the part's output at `scale`, kept in step with the session through
    /// its link; `waker` wakes the session's loop when work the instance waits for is ready.
    ///
    /// The views' wells ([`ui::widget::Wells`]) report to `wells`, the handle the window policy
    /// placing the windows they stand for shares (`crate::WellPolicy`).
    ///
    /// The part is the desk's whole screen ([`screen_layer`]): it shows the person's
    /// arrangement and puts each window in its well.
    #[must_use]
    pub fn new(
        identity: ShellIdentity<'_>,
        scale: f64,
        waker: Waker,
        wells: Option<ui::widget::Wells>,
    ) -> Self {
        let ShellIdentity {
            app,
            packages,
            person,
            link,
            record,
            seed,
        } = identity;
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
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
                // The desk's whole screen, not whatever root the package lists first.
                root: screen_layer(),
            },
            Rc::new(ShellPlatform {
                packages: Rc::new(packages),
                waker,
                link: Rc::clone(&link),
            }),
            Rc::new(Person(person.to_owned())),
            Box::new(app_end),
        );
        instance.set_standing(Rc::new(shell_standing(app, person)));
        let origin = Rc::new(RefCell::new(SessionOrigin::new(link)));
        instance.add_origin(Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin);
        {
            let record = record.borrow();
            instance.sync(Some(&record));
        }
        let window = AppWindow::part(Box::new(window_end));
        // The whole screen lies under the windows, which stand in its wells; the bar rides over
        // them.
        let mut widgets = WidgetHost::new(Box::new(window), placement, true).named("Desktop shell");
        widgets.install_wells(wells);
        let mut part = Self {
            widgets,
            instance,
            record,
            origin,
            input: false,
            again: true,
            work: ShellWork::default(),
            refusals: Vec::new(),
            output: None,
            output_scale: scale,
            gone: None,
            fullscreen: Vec::new(),
            cover: false,
            seen_heads: Vec::new(),
            previews: None,
            deadline: None,
            content: None,
            content_workspace: String::new(),
        };
        if seed {
            part.seed();
        }
        part.turn();
        // The Rust apps' views linked into this process: a place showing a resource of one of
        // their types draws that app's own view, exactly as it draws a tilolang package's
        // (one-app-model.md §2). The runtime is up after the first turn, when the host is made.
        if let Some(host) = part.instance.host() {
            host.declare_rust_views();
        }
        // What a composed view opens is a place on the canvas, never a lane of the shell's own
        // window.
        part.instance.place_opens(true);
        part
    }

    /// Make the shell's own record (`root shell: Shell`) and let the package's migration seed
    /// it — the bar's fields and the dock's items, on a shell never arranged — as the package
    /// version's change, as an app's migration is. Both go through the part's own turns: the
    /// record is created as a queued change, the migration runs once the record is had, and
    /// [`Self::commit`] writes each into the part's record.
    fn seed(&mut self) {
        let Some(host) = self.instance.host() else {
            return;
        };
        let roots = host
            .package(self.instance.app())
            .map(|package| package.meta.named_roots)
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
            {
                let record = self.record.borrow();
                self.instance.sync(Some(&record));
            }
            self.instance.commit_planned();
            if !self.commit() {
                break;
            }
        }
        // The seed moved the record: the part's next turn settles what it started (a view
        // that read a bound field runs again), as any change of the record does.
        self.instance.moved();
        // What the seed wrote is the shell's for every part: the heads here are the ones a
        // part made later finds and takes in.
        self.seen_heads = self
            .record
            .borrow()
            .committed_heads()
            .iter()
            .map(ToString::to_string)
            .collect();
    }

    /// Stand on this part's whole output: the desk is the screen, its bar and dock over the
    /// places the windows stand in.
    ///
    /// A part on an output a fullscreen window covers takes no room and draws nothing: the
    /// window owns the display there, as it owns the desktop's own bar's display.
    fn place(&mut self) {
        let Some((name, (x, y, width, output_height))) = self.output.clone() else {
            return;
        };
        let scale = self.output_scale;
        if self.fullscreen.contains(&name) && !self.cover {
            // Nothing of the part stands there: the smallest placement keeps its layout cheap,
            // and `hidden` hands the compositor no layer and takes no pointer, so not a row of
            // it lies over the window.
            self.widgets.set_placement(Placement {
                origin: (x, y),
                size: (width.max(1), 1),
                scale,
            });
            self.widgets.relayout();
            return;
        }
        self.widgets.set_placement(Placement {
            origin: (x, y),
            size: (width.max(1), output_height.max(1)),
            scale,
        });
        self.widgets.relayout();
    }

    /// Follow whether the part stands over the whole outputs: while a menu it opened is up,
    /// or while a surface of the desk's is (the overview, the switcher, the launcher, the
    /// notifications, the workspaces, the composer).
    fn follow_cover(&mut self) {
        // A menu node a view lays out closed is hidden, and a place's frame carries one: counting
        // it would stand the part over every window for as long as a frame is drawn, so no press
        // or key would ever reach the window in one.
        let menu = self
            .widgets
            .host_mut()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.role() == ui::accesskit::Role::Menu && !node.is_hidden());
        let surface = !matches!(
            self.origin.borrow().link.surface().as_str(),
            "" | "none" | "desk"
        );
        let cover = menu || surface;
        if cover != self.cover {
            self.cover = cover;
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

    /// Where every control assistive technology names `name` stands: a menu item repeats the
    /// name of the row it moves a place to, which a chip of that row elsewhere says too.
    pub fn control_bounds_all(&mut self, name: &str) -> Vec<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some(name))
            .filter_map(|(_, node)| node.bounds())
            .map(|rect| {
                (
                    f64::from(ox) + rect.x0,
                    f64::from(oy) + rect.y0,
                    rect.width(),
                    rect.height(),
                )
            })
            .collect()
    }

    /// Whether anything the shell lays out for assistive technology says `text`: a node's
    /// name, description or value holds it. What a test reads a composed view's content by.
    pub fn says(&mut self, text: &str) -> bool {
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .any(|(_, node)| {
                [node.label(), node.description(), node.value()]
                    .into_iter()
                    .flatten()
                    .any(|said| said.contains(text))
            })
    }

    /// Where the first node that says `text` (its name, description or value holds it) stands,
    /// in the desktop's logical layout `(x, y, width, height)`.
    pub fn text_bounds(&mut self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.text_bounds_all(text).into_iter().next()
    }

    /// Where the first node that says `text` is *drawn*: its own box, cut to the clip of the
    /// scroll view it stands in, in the desktop's logical layout `(x, y, width, height)`.
    ///
    /// A transcript's rows are laid out whole and drawn through their scroll view's viewport: a
    /// row scrolled above it has a box outside it and no pixels inside it. What this answers is
    /// where the row's pixels can be — the intersection of its box with every clip between it
    /// and the screen — so a test asks what the person sees, never where the layout put a row
    /// that the view cuts off. `None` where the node says `text` but is drawn nowhere.
    pub fn drawn_text_bounds(&mut self, text: &str) -> Option<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        let root = self.widgets.host_mut().root_widget_mut();
        let found = drawn_text(root, text)?;
        let rect = match found.clip {
            Some(cut) => found.rect.intersect(cut),
            None => found.rect,
        };
        (rect.width() > 0.0 && rect.height() > 0.0).then(|| {
            (
                f64::from(ox) + rect.x0,
                f64::from(oy) + rect.y0,
                rect.width(),
                rect.height(),
            )
        })
    }

    /// The clip a scroll view draws its content through, as the room a transcript has under a
    /// page's header: where the first scroll on screen whose content says `text` draws. `None`
    /// where nothing says it or no scroll holds it.
    pub fn scroll_clip(&mut self, text: &str) -> Option<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        let root = self.widgets.host_mut().root_widget_mut();
        let found = drawn_text(root, text)?;
        let clip = found.clip?;
        Some((
            f64::from(ox) + clip.x0,
            f64::from(oy) + clip.y0,
            clip.width(),
            clip.height(),
        ))
    }

    /// Where every node that says `text` (its name, description or value holds it) stands, in the
    /// desktop's logical layout `(x, y, width, height)`. A name more than one thing on the desk
    /// carries — a strip's name, which the bar's pill and nothing on the desk says — is read by all
    /// of them, so what a test asks of a name is what the whole desk says of it.
    pub fn text_bounds_all(&mut self, text: &str) -> Vec<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| {
                [node.label(), node.description(), node.value()]
                    .into_iter()
                    .flatten()
                    .any(|said| said.contains(text))
            })
            .filter_map(|(_, node)| node.bounds())
            .map(|rect| {
                (
                    f64::from(ox) + rect.x0,
                    f64::from(oy) + rect.y0,
                    rect.width(),
                    rect.height(),
                )
            })
            .collect()
    }

    /// The apps whose packages the part's runtime holds loaded, because a place shows them.
    #[must_use]
    pub fn loaded_apps(&self) -> Vec<String> {
        self.origin.borrow().loaded.keys().cloned().collect()
    }

    /// The views that could not be shown since the last call, each an app, a view and why: what
    /// the session hears so it can say so where the person's own choice is the one that failed
    /// (`shell.md` §6, "Nothing can lock a person out").
    #[must_use]
    pub fn take_view_failures(&self) -> Vec<tilolang_tilo::runtime::foreign::ViewFailure> {
        self.origin.borrow().link.take_view_failures()
    }

    /// The apps whose view failure ended since the last call, and the view calls that ran again
    /// and succeeded: what a style the person chose comes back from (`shell.md` §6, "Nothing
    /// can lock a person out").
    #[must_use]
    pub fn take_view_recoveries(&self) -> (Vec<String>, Vec<(String, String)>) {
        self.origin.borrow().link.take_view_recoveries()
    }
    /// Draw the shell with `theme`, the person's as their appearance settings make it: a
    /// change of mode (dark mode in the quick settings, a theme chosen in Settings) redraws
    /// the bar and every surface over the desk in it.
    pub fn set_theme(&mut self, theme: ui::Theme) {
        self.widgets.set_theme(theme);
        self.again = true;
    }

    /// Show the live pictures of the windows the part's views name
    /// ([`ui::widget::WindowPreview`]): each preview's rectangle is where the window it names
    /// stands on this display, and `windows` says which of the compositor's windows that key
    /// is — `None` for a window that may not be pictured, which shows its view's own mark
    /// instead.
    pub fn show_pictures(&mut self, previews: ui::widget::Previews, windows: WindowLookup) {
        self.widgets.install_previews(Some(previews.clone()));
        self.previews = Some((previews, windows));
    }

    /// Read its places' pictures from `content`, the session's document media as the workspace
    /// `workspace` in front holds it: a place shows the records an app's connector kept stills
    /// for, and the part draws in the session's own process, which the launcher handed no store
    /// of its own.
    ///
    /// The store keeps every workspace the session draws, so one lending serves a place
    /// whichever workspace it stands in; a part told the same store twice keeps the one it has.
    pub fn lend_content(&mut self, workspace: &str, content: Option<ui::content::ContentStore>) {
        if self.content_workspace == workspace && self.content.is_some() == content.is_some() {
            return;
        }
        workspace.clone_into(&mut self.content_workspace);
        self.content = content;
        // The views asked for their pictures before one answered: the frames after this ask
        // again, now that a store reads them.
        self.again = true;
    }

    /// When the session has something of its own to draw, which the part does not know: it
    /// wakes at `deadline` rather than sleeping over it, and draws what the session publishes
    /// by then.
    pub fn set_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.deadline = deadline;
    }

    /// Stand the part on `output`, its connector name and logical rectangle `(x, y, w, h)`:
    /// the part is one display's, and follows that display as it is added, moved or turned
    /// off. Every part of one shell is told its own display; none spans them all. `None`: the
    /// display went, and the part stands nowhere until it is told one again.
    pub fn set_display(&mut self, output: Option<(String, (i32, i32, u32, u32))>, scale: f64) {
        if self.output == output && self.output_scale.to_bits() == scale.to_bits() {
            return;
        }
        if output.is_some() {
            self.gone = None;
        } else {
            // What it drew goes with its display: nothing of it stands anywhere now.
            self.gone = self.output.take().map(|(name, _)| name);
            self.cover = false;
        }
        self.output = output;
        self.output_scale = scale;
        self.place();
    }

    /// Whether the display the part stood on went, and it stands nowhere now.
    fn retired(&self) -> bool {
        self.gone.is_some()
    }

    /// Whether the part shows nothing at all now: its display went, or a fullscreen window
    /// owns it. It has no layers and takes no pointer then.
    fn hidden(&self) -> bool {
        self.retired() || self.aside()
    }

    /// Tell the part which output's or outputs' windows cover the whole display, by connector
    /// name: the part on one of them steps aside and draws nothing, as the desktop's own bar
    /// does where a window owns its display. A change steps it back when they go.
    pub fn set_fullscreen(&mut self, outputs: Vec<String>) {
        if self.fullscreen == outputs {
            return;
        }
        self.fullscreen = outputs;
        self.place();
    }

    /// Where the part stands now, in the desktop's logical layout `(x, y, w, h)`, and the
    /// display it names: `None` until it is told of its output.
    #[must_use]
    pub fn placement(&self) -> Option<(String, (i32, i32, u32, u32))> {
        self.output.clone()
    }

    /// The scale the part draws at: its display's.
    #[must_use]
    pub const fn part_scale(&self) -> f64 {
        self.output_scale
    }

    /// Whether the part draws nothing now: it stands on a display a fullscreen window owns.
    #[must_use]
    pub fn aside(&self) -> bool {
        self.output.as_ref().is_some_and(|(name, _)| {
            !self.cover && self.fullscreen.iter().any(|covered| covered == name)
        })
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

    fn window_ref(&self) -> Option<&AppWindow> {
        self.widgets
            .host()
            .root_widget()
            .as_any()?
            .downcast_ref::<AppWindow>()
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
                .and_then(|host| Some((host.store().take_ticket(ticket)?, host)))
                .ok_or_else(|| format!("no committed action {ticket}"))
                .and_then(|(action, host)| {
                    crate::commit::recorded(&host, &mut self.record.borrow_mut(), &action)
                });
            if let Err(why) = &applied {
                tracing::error!(why, "a shell change could not be committed");
            }
            self.instance.answered(&address, applied.err());
            committed = true;
        }
        if committed {
            // The record holds the changes once they are committed: the instance reads them
            // from its heads, and every view runs again, since the change is the record's.
            {
                let mut record = self.record.borrow_mut();
                record.commit();
                self.seen_heads = record
                    .committed_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
            }
            let at = std::time::Instant::now();
            let record = self.record.borrow();
            self.instance.sync(Some(&record));
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
            // An index built off the frame is taken in here, as a window's turn takes it in: the
            // shell's own views read what it holds, so the turn that takes it in draws them. A
            // shell whose indexes are all read from (every frame after the open) asks nothing.
            if self.index_building() {
                let record = self.record.borrow();
                self.instance.sync(Some(&record));
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
        // A change another display's part of this shell committed reaches this one: the record
        // it shares moved on from the heads this instance saw, and it draws the shell as it is
        // now. Nothing else changed, so no other frame is owed.
        let mut record_moved = false;
        {
            let heads: Vec<String> = self
                .record
                .borrow()
                .committed_heads()
                .iter()
                .map(ToString::to_string)
                .collect();
            if heads != self.seen_heads {
                self.seen_heads = heads;
                let record = self.record.borrow();
                self.instance.sync(Some(&record));
                self.instance.moved();
                record_moved = true;
            }
        }
        if record_moved {
            // The views that read a bound field run again in this turn, so the part draws the
            // shell's change now rather than a frame late.
            let mut at = std::time::Instant::now();
            self.instance.update();
            self.work.updating = self.work.updating.saturating_add(at.elapsed());
            at = std::time::Instant::now();
            shown |= self.window().is_some_and(AppWindow::receive);
            self.work.receiving = self.work.receiving.saturating_add(at.elapsed());
            if shown {
                self.widgets.relayout();
                self.work.laying_out = self.work.laying_out.saturating_add(at.elapsed());
            }
        }
        // What the views composed in places read of other apps' resources before their
        // readings came: the session is asked to publish those. What they read of a type no
        // loaded package declared: the session is asked for that app's package.
        if let Some(host) = self.instance.host() {
            let wanted = host.runtime().take_wanted();
            if !wanted.is_empty() {
                self.origin.borrow().link.want(wanted);
            }
            let packages: BTreeMap<String, String> =
                host.runtime().take_wanted_packages().into_iter().collect();
            if !packages.is_empty() {
                self.origin.borrow().link.want_packages(packages);
            }
            // What the views could not show — a package that did not load, a view that failed
            // — so the session says so where it is the person's choice that failed
            // (`shell.md` §6, "Nothing can lock a person out").
            let failed = host.runtime().take_view_failures();
            if !failed.is_empty() {
                self.origin.borrow().link.view_failed(failed);
            }
            // An app whose package failed to load and now loaded: its style draws again.
            let recovered = host.runtime().take_recovered_apps();
            if !recovered.is_empty() {
                self.origin.borrow().link.view_recovered(recovered);
            }
            // A call that failed and now runs: the style it draws is back.
            for failure in host.runtime().take_recovered_views() {
                self.origin
                    .borrow()
                    .link
                    .view_recovered_call((failure.app, failure.view));
            }
        }
        // What a composed view opened (a video row pressed on a channel's page): a place the
        // session arranges on the canvas, one step of the person's, never a detail lane of the
        // shell's own window (one-app-model.md §2, shell.md §8).
        self.place_opens();
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
        let notices = window.take_notices();
        // A window that refused what the view sent shows the platform's error state: that
        // is the shell refusing to show itself, heard once.
        let failed = window.failed().map(str::to_owned);
        for (kind, text) in notices {
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
        if let Some(why) = failed
            && !self.refusals.contains(&why)
        {
            self.refusals.push(why);
        }
    }

    /// What the shell refused since it started, in its own words.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refusals
    }

    /// Whether the window holds something for the instance it has not sent: the space a layout
    /// gave a view, or what the person did. The part turns to hand it over, so a view is told
    /// the space it has without anything being touched, rather than at the person's next press.
    fn owes(&self) -> bool {
        self.window_ref().is_some_and(AppWindow::owes)
    }

    fn due(&self) -> bool {
        self.input
            || self.again
            || self.owes()
            || self.origin.borrow().due()
            || self.record_moved()
            || self.index_building()
    }

    /// Whether the shell's instance still waits for the index its packages' load builds off the
    /// frame: until a frame takes it in the views read nothing, and nothing else wakes the part
    /// for it — a hosted app's window is asked for its index's frame, and the shell, which draws
    /// its own, asks for the frames itself.
    fn index_building(&self) -> bool {
        self.instance.host().is_some_and(|host| !host.index_ready())
    }

    /// Whether the shell's record holds changes this part has not drawn: another part of the
    /// shell committed them, or a connection the session admits (a REPL, an agent) changed the
    /// record the shell's own actions are published on, and the part draws them next frame.
    fn record_moved(&self) -> bool {
        let record = self.record.borrow();
        let heads: Vec<String> = record
            .committed_heads()
            .iter()
            .map(ToString::to_string)
            .collect();
        heads != self.seen_heads
    }
}

impl crate::Published for ShellPart {
    /// A part whose display went offers nothing: the parts still standing answer the door.
    fn offered(&self) -> bool {
        !self.retired()
    }

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
        look: &mut crate::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        self.input = true;
        crate::Published::invoke(&mut self.widgets, address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        crate::Published::accessible_labels(&mut self.widgets)
    }

    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        crate::Published::controls(&mut self.widgets)
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        // A press takes the part into its turn, as a press of the pointer does: a named press
        // (an assistive technology's activation, `tilo-control click`) changes the shell's
        // record, and the change is drawn by the turn that takes it in. The part wakes for
        // input alone, so a press that did not say so waited for the next deadline — the
        // clock — the desk drawing the style the person left until then.
        self.input = true;
        crate::Published::press(&mut self.widgets, name)
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        crate::Published::a11y_tree(&mut self.widgets)
    }

    fn a11y_name(&self) -> Option<String> {
        crate::Published::a11y_name(&self.widgets)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        self.input = true;
        crate::Published::a11y_action(&mut self.widgets, target, action)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
    }
}

/// The keysym of Tab, as the layout reports it: Alt+Tab and Alt+Shift+Tab walk the switcher.
const XKB_TAB: u32 = 0xff09;
/// The keysym of `Alt_L`.
const XKB_ALT_L: u32 = 0xffe9;
/// The keysym of the key above Tab: `` ` ``, and `~` with Shift held.
const XKB_GRAVE: [u32; 2] = [0x0060, 0x007e];

/// What a press means to the shell's surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurfaceKey {
    /// Alt+Tab: open the switcher on the window before the one in front, walking on from
    /// there (back with Shift); the same over the app in front's windows with Alt and the grave
    /// key.
    Walk { back: bool, app_scope: bool },
    /// Enter, or letting Alt go while the switcher is up: go to the window it reached.
    Commit,
    /// Esc: close the surface that is up.
    Close,
}

impl ShellPart {
    /// What `key` means to the shell while a surface of the desk's is up, or as a press that
    /// opens one. `None` when it belongs to whatever has the keyboard.
    fn surface_key(&self, key: &HostKey) -> Option<SurfaceKey> {
        if key.pressed && key.modifiers.alt {
            let app_scope = XKB_GRAVE.contains(&key.keysym);
            if key.keysym == XKB_TAB || app_scope {
                return Some(SurfaceKey::Walk {
                    back: key.modifiers.shift,
                    app_scope,
                });
            }
        }
        let up = self.origin.borrow().link.surface();
        if matches!(up.as_str(), "" | "none" | "desk") {
            return None;
        }
        if up == "switcher" && matches!(key.keysym, XKB_ALT_L | 0xffea) && !key.pressed {
            return Some(SurfaceKey::Commit);
        }
        match key.keysym {
            0xff0d if key.pressed && up == "switcher" => Some(SurfaceKey::Commit),
            0xff1b if key.pressed => Some(SurfaceKey::Close),
            _ => None,
        }
    }
}

impl ShellPart {
    /// Ask the session for the desk action `action` with `arguments`: what a key of the part's
    /// own asks, as a press of the views asks through the session's door.
    fn ask(&self, action: &str, arguments: &[(&str, serde_json::Value)]) {
        let arguments = arguments
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        self.origin.borrow().link.ask(Ask {
            resource: DESK_RESOURCE.to_owned(),
            action: action.to_owned(),
            arguments,
            from: None,
        });
    }

    /// What a composed view opened since the last turn, each placed on the canvas through the
    /// arrangement's own `place`, one arranging step of the person's: a video row pressed on a
    /// channel's page stands the video beside it (one-app-model.md §2, shell.md §8).
    fn place_opens(&mut self) {
        let opened = self.instance.take_opens_to_place();
        if opened.is_empty() {
            return;
        }
        for (target, view, _front) in opened {
            let mut arguments = BTreeMap::new();
            arguments.insert("resource".to_owned(), target.id_string().into());
            if let Some(view) = view {
                arguments.insert("view".to_owned(), view.into());
            }
            self.origin.borrow().link.ask(Ask {
                resource: ARRANGEMENT_RESOURCE.to_owned(),
                action: "place".to_owned(),
                arguments,
                from: None,
            });
        }
    }

    /// Draw a frame of the part's widget tree when it owes one, and say whether it drew or
    /// changed what it shows. What it owes a frame for is what the views said (a turn that was
    /// shown lays the tree out), input, a motion or a deadline. A field on screen reports
    /// itself dirty every frame, so that alone draws nothing: with the centre or the launcher
    /// up and nothing changing, the part draws no frame.
    #[allow(dead_code, reason = "a test draws a frame between presses")]
    fn draw(&mut self) -> bool {
        if !self.widgets.wants_frame() {
            return false;
        }
        let before = crate::Published::revision(&self.widgets);
        let started = std::time::Instant::now();
        let drew = self.widgets.prepare_frame();
        let changed = crate::Published::revision(&self.widgets) != before;
        if changed {
            self.work.drawn = self.work.drawn.wrapping_add(1);
            self.work.drawing = self.work.drawing.saturating_add(started.elapsed());
        }
        drew || changed
    }
}

pub mod readings;
use readings::PlaceInvoker;

/// Where a node that says `text` stands in `widget`'s tree, and the clip every enclosing scroll
/// draws it through: the node's own box, and the intersection of each scroll view's content
/// viewport on the way down. What a test asks is what the person sees, so a row the transcript
/// has scrolled out of its room is drawn nowhere, never over the header above it.
struct DrawnText {
    rect: vello::kurbo::Rect,
    clip: Option<vello::kurbo::Rect>,
}

fn drawn_text(widget: &dyn ui::widget::Widget, text: &str) -> Option<DrawnText> {
    fn says(widget: &dyn ui::widget::Widget, text: &str) -> Option<vello::kurbo::Rect> {
        let access = widget.accessibility()?;
        let held = [
            access.name.as_deref(),
            access.description.as_deref(),
            access.value.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|said| said.contains(text));
        held.then(|| {
            access
                .bounds
                .map_or_else(|| widget.get_rect(), ui::a11y::AccessBounds::to_rect)
        })
    }
    fn walk(
        widget: &dyn ui::widget::Widget,
        text: &str,
        clip: Option<vello::kurbo::Rect>,
    ) -> Option<DrawnText> {
        // This widget's own clip applies to its children; a node inside it is drawn through it.
        let clip = match widget.child_clip_rect() {
            Some(own) => Some(clip.map_or(own, |outer| outer.intersect(own))),
            None => clip,
        };
        if let Some(rect) = says(widget, text) {
            return Some(DrawnText { rect, clip });
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| walk(child, text, clip))
    }
    walk(widget, text, None)
}
#[cfg(test)]
mod tests_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod places;

#[cfg(test)]
mod surface_keys;
