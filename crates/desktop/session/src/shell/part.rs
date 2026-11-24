//! A part of a tilolang shell, covering one output, drawn under the windows.
//!
//! A shell package's views run in a tilolang instance (`tilolang_tilo::instance`), exactly as
//! an app's do, and draw through the same app window half (`tilo_app_window::AppWindow`, here as
//! a [`AppWindow::part`]: the view as it is, no window shell). The toolkit lays that widget tree
//! out and the CPU rasterizes it, as [`WidgetSurface`] does for every widget tree the session
//! shows; the compositor gets the buffer as a layer. Nothing here renders on its own.
//!
//! What the views read of the session comes in as the person's layout, whose origin is the
//! session (`type Layout (origin: session.layout)`), which [`SessionOrigin`] writes,
//! and as the readings of the device's resources (`device.overlays`). The views call the
//! session's actions as an app's (`use so.tilo.session as session`, then `session.overview()`),
//! each an invocation on its API ([`PlaceInvoker`]). A shell view never reaches the session
//! another way.
//!
//! The part works only when something changed: a press, a reading of the session's that moved,
//! or an API answer. Otherwise a frame asks nothing of the instance, and the toolkit draws
//! nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_session_core::surface::{CursorIcon, KeyClaim, SurfaceKey};
use tilo_session_core::surface::{ShellSurface, SurfaceIme, SurfaceLayer, SurfaceTextInput};

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode};
use tilolang_tilo::origin::{Origin, OriginCx, Polled};
use tilolang_tilo::platform_apps::platform_packages;
use tilolang_tilo::resources::{ResourceStore, Watch};
use tilolang_tilo::runtime::Delivery;
use tilolang_tilo::tilolang::Compiled;

use crate::platform::session_platform::{Loads, OnSession};
use crate::platform::surfaces::{Placement, WidgetSurface};

/// What a shell reads of the session and the names the session publishes it by: every host's,
/// so the shell's views and the session use the same names.
pub use tilo_session_core::shell::{
    Ask, CHROME_RESOURCE, LAYOUT_RESOURCE, LayoutPersonReading, LayoutPlaceReading,
    LayoutQueryReading, LayoutReading, LayoutRecentReading, LayoutRefReading, LayoutRowReading,
    LayoutStripReading, LayoutWorkspaceReading, ORIGIN, OVERLAYS, SHELL_APP, SHELL_RECORD,
};

mod settings;
mod surface;

use tilolang_tilo::shell::{AskedReading, AskingWriter, LayoutWriter};
/// The shell's package, as every host compiles it.
pub use tilolang_tilo::shell::{
    catalogues as shell_catalogues, compile as compile_shell, sources as shell_sources,
};

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

/// The shell's whole screen: the `shell_view` view over the person's layout (the package's
/// `layout` root), which stands the shell's bar, dock and surfaces around the places as the
/// preset the person picked lays them out. Each place an app's window shows in is a well, and
/// the window stands where that well does.
#[must_use]
pub fn screen_layer() -> RootView {
    RootView {
        root: Some("layout".to_owned()),
        view: Some("shell_view".to_owned()),
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
        out.push(compiled_of(&import.wasm, &import.meta, "")?);
    }
    out.push(compiled_of(&bundle.wasm, &bundle.meta, &bundle.ids)?);
    Ok(Some(out))
}

/// A compiled package made of the module `wasm`, the metadata `meta` and the ids lock `ids`,
/// as an installed payload holds them: the shape a shell's instance loads.
///
/// # Errors
/// The payload's ids lock does not read.
fn compiled_of(
    wasm: &[u8],
    meta: &tilolang_tilo::tilolang::PackageMeta,
    ids: &str,
) -> Result<Compiled, String> {
    let ids = tilolang_tilo::tilolang::IdsLock::parse(ids)
        .map_err(|error| format!("{SHELL_APP}: its ids lock: {error}"))?;
    Ok(Compiled {
        wasm: wasm.to_vec(),
        meta: meta.clone(),
        ids,
    })
}

/// How often the instance and its window may answer each other in one frame before the frame
/// is drawn as it is: a press, the action it ran, the change it committed, the effect settled.
const EXCHANGES: usize = 8;

/// Between the session and the shell's origin: the readings the session last published, what the
/// shell asked of the API, and the API's answers. The session serves it every turn
/// of its loop ([`SessionLink::serve`]); the origin reads and writes the rest.
#[derive(Default)]
pub struct SessionLink {
    /// The person's layout the session last published.
    layout: RefCell<Option<LayoutReading>>,
    /// How often the session published the layout. Every part applies each generation once: a
    /// part reads it on its own runtime, so a second display's part never misses one the first
    /// part took.
    layout_generation: Cell<u64>,
    /// What waits for the person's yes, as the session last read it, and how often it was
    /// published: every part applies each generation once, as with the layout.
    asking: RefCell<Vec<AskedReading>>,
    asking_generation: Cell<u64>,
    /// The readings of the resources the places name, by their session id: each with its type
    /// as the shell's runtime names another app's (`<app>#<Type>`) and its JSON.
    readings: RefCell<BTreeMap<String, (String, serde_json::Value)>>,
    /// How often each resource's reading was published, by id: every part applies each
    /// generation once, for the same reason.
    readings_generation: RefCell<BTreeMap<String, u64>>,
    /// The packages of the apps whose resources the places show, by app id: each its compiled
    /// module, which the shell's runtime loads to draw those resources through their app's
    /// own views, and unloads once no place shows that app.
    packages: RefCell<BTreeMap<String, std::sync::Arc<[u8]>>>,
    packages_generation: Cell<u64>,
    /// The verbs the session's resources publish that no loaded package declares, by resource
    /// and verb ([`tilolang_tilo::runtime::published`]): what the shell's runtime sends to the app
    /// publishing them. `None` until the session publishes one.
    published: RefCell<Option<tilolang_tilo::runtime::published::Published>>,
    published_generation: Cell<u64>,
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
    /// What the API refused of what the shell asked, in its words: each refusal, kept for
    /// whoever asks ([`SessionLink::refused`]), never a quiet drop.
    refused: RefCell<Vec<String>>,
}

impl SessionLink {
    /// The session published `reading`, the person's layout as it stands now: the shell
    /// writes it on its next frame when it differs from the last. Whether it did.
    pub fn publish_layout(&self, reading: LayoutReading) -> bool {
        if self.layout.borrow().as_ref() == Some(&reading) {
            return false;
        }
        *self.layout.borrow_mut() = Some(reading);
        self.layout_generation
            .set(self.layout_generation.get().wrapping_add(1));
        true
    }

    /// The session read `questions`, what waits for the person's yes now: the shell writes them
    /// on its next frame when they differ from the last. Whether they did.
    pub fn publish_asking(&self, questions: Vec<AskedReading>) -> bool {
        if *self.asking.borrow() == questions {
            return false;
        }
        *self.asking.borrow_mut() = questions;
        self.asking_generation
            .set(self.asking_generation.get().wrapping_add(1));
        true
    }

    /// Run what the shell asked through `invoke` (the session's API), keeping each refusal.
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

    /// What the API refused of what the shell asked since the session started, in its words.
    #[must_use]
    pub fn refused(&self) -> Vec<String> {
        self.refused.borrow().clone()
    }

    /// The shell asks `ask` of the session's API: run on the session's next turn
    /// ([`Self::serve`]). One of the device's settings a view names is this device's.
    pub fn ask(&self, mut ask: Ask) {
        ask.resource = tilolang_tilo::platform_apps::on_device(
            &ask.resource,
            &ui::settings_device::settings_device(),
        );
        self.asked.borrow_mut().push_back(ask);
        self.handed.set(self.handed.get().wrapping_add(1));
    }

    /// Whether the shell asked something the session has not run yet.
    #[must_use]
    pub fn has_asks(&self) -> bool {
        !self.asked.borrow().is_empty()
    }

    /// The surface the shell shows now (`none` at rest): what the part covers the outputs for.
    #[must_use]
    pub fn surface(&self) -> String {
        self.readings
            .borrow()
            .get(OVERLAYS)
            .and_then(|(_, reading)| reading.get("here")?.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// The session as the origin of a shell's `session.*` resources. See the module docs.
pub struct SessionOrigin {
    link: Rc<SessionLink>,
    /// What it wrote of the person's layout into the shell's `Layout` root.
    layout: LayoutWriter,
    /// What it wrote of the questions into the shell's `Asking` root.
    asking: AskingWriter,
    /// The packages of other apps loaded into the shell's runtime, by app id: each the module
    /// the session handed for it.
    loaded: BTreeMap<String, std::sync::Arc<[u8]>>,
    /// The layout generation this part applied last, and each reading's: what a part has read of
    /// what the session published, so a second display's part reads every one too.
    layout_seen: Cell<u64>,
    asking_seen: Cell<u64>,
    readings_seen: RefCell<BTreeMap<String, u64>>,
    packages_seen: Cell<u64>,
    published_seen: Cell<u64>,
    /// The display this part draws on, as this part's own reading (`session:screen`), and how
    /// often it was told one: the part's views draw its display's strips and bar, so they read
    /// their own.
    screen: RefCell<Option<serde_json::Value>>,
    screen_generation: Cell<u64>,
    screen_seen: Cell<u64>,
}

impl SessionOrigin {
    #[must_use]
    pub fn new(link: Rc<SessionLink>) -> Self {
        Self {
            loaded: BTreeMap::new(),
            link,
            layout: LayoutWriter::default(),
            asking: AskingWriter::default(),
            layout_seen: Cell::new(0),
            asking_seen: Cell::new(0),
            readings_seen: RefCell::new(BTreeMap::new()),
            packages_seen: Cell::new(0),
            published_seen: Cell::new(0),
            screen: RefCell::new(None),
            screen_generation: Cell::new(0),
            screen_seen: Cell::new(0),
        }
    }

    /// This part stands on `display`: its views read it as their own (`device.screen`), so what
    /// they draw is that display's strips and bar.
    pub fn set_screen(&mut self, display: Option<serde_json::Value>) {
        if *self.screen.borrow() == display {
            return;
        }
        *self.screen.borrow_mut() = display;
        self.screen_generation
            .set(self.screen_generation.get().wrapping_add(1));
    }
}

impl SessionOrigin {
    /// Write the person's layout into the shell's `Layout` root, when the session
    /// published one the origin has not written yet. Whether anything was written.
    fn poll_layout(&mut self, cx: &OriginCx<'_>) -> bool {
        let generation = self.link.layout_generation.get();
        if generation <= self.layout_seen.get() {
            return false;
        }
        self.layout_seen.set(generation);
        let Some(reading) = self.link.layout.borrow().clone() else {
            return false;
        };
        self.layout.write(cx, &reading)
    }

    /// Write what waits for the person's yes into the shell's `Asking` root, when the session
    /// read questions the origin has not written yet. Whether anything was written.
    fn poll_asking(&mut self, cx: &OriginCx<'_>) -> bool {
        let generation = self.link.asking_generation.get();
        if generation <= self.asking_seen.get() {
            return false;
        }
        self.asking_seen.set(generation);
        self.asking.write(cx, &self.link.asking.borrow())
    }
}

impl Origin for SessionOrigin {
    fn name(&self) -> &str {
        ORIGIN
    }

    /// The shell calls the session's actions as an app's (`session.show_surface(...)`), each an
    /// invocation on its API ([`PlaceInvoker`]); no outside effect is the session's.
    fn serves(&self, _capability: &str) -> bool {
        false
    }

    fn due(&self) -> bool {
        self.link.layout_generation.get() > self.layout_seen.get()
            || self.link.asking_generation.get() > self.asking_seen.get()
            || self.link.packages_generation.get() > self.packages_seen.get()
            || self.link.published_generation.get() > self.published_seen.get()
            || self.screen_generation.get() > self.screen_seen.get()
            || self
                .link
                .readings_generation
                .borrow()
                .iter()
                .any(|(id, generation)| {
                    *generation > self.readings_seen.borrow().get(id).copied().unwrap_or(0)
                })
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let mut polled = Polled {
            wrote: false,
            settle: false,
        };
        let wrote_layout = self.poll_layout(cx);
        polled.wrote |= wrote_layout;
        polled.wrote |= self.poll_asking(cx);
        // The packages the places need first: a reading of a resource is read as its app's
        // package declares it.
        polled.wrote |= self.poll_packages(cx);
        polled.wrote |= self.poll_readings(cx);
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
    /// Where what the composed views change of other apps' resources goes.
    link: Rc<SessionLink>,
}

impl Loads for ShellPlatform {
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
                        tracing::error!(%app, %error, "a platform package did not load");
                        // A view that names this package's resources while it is missing draws
                        // a failed state: it says which package failed and why, never a blank
                        // card with no reason.
                        host.runtime().app_failed(&app, error.to_string());
                    }
                }
            }
            Err(why) => tracing::error!(why, "the platform's packages do not read"),
        }
        // Other apps' packages come as places show their resources ([`SessionOrigin`]): their
        // records are the ones their apps keep, read through the session
        // ([`SessionLink::follow_places`]), and what their views change goes to those records'
        // APIs ([`PlaceInvoker`]). The shell's record holds the shell's own records only.
        let Some(shell) = self.packages.last() else {
            return 0;
        };
        // The session's own package is a description: its actions run at the session's API.
        // It is the one the shell was built against — the bundle's own import, whose action ids
        // are the ids the shell's code calls (`Desktop/workspace-menu`). A package read again
        // from the source describes the same resources under the same names, but names its
        // actions as the surface does (`workspace-menu`), which is not what a compiled call names:
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
                    host.runtime().app_failed(SESSION_APP, error.to_string());
                }
            }
            Err(error) => {
                tracing::error!(%error, "the session's description does not read");
                host.runtime().app_failed(SESSION_APP, error);
            }
        }
        if let Err(error) = host.load_compiled(shell) {
            tracing::error!(app = shell.meta.app, %error, "the shell package did not load");
            host.runtime()
                .app_failed(&shell.meta.app, error.to_string());
        }
        host.runtime().set_invoker(PlaceInvoker {
            link: Rc::clone(&self.link),
            session: session_addresses(),
            verbs: session_verbs(),
        });
        0
    }
}

/// The room a shell's record is decided in: the person's own, on this device.
const SHELL_ROOM: &str = "shell";

/// `person`'s access over their own shell `app`: they own its record, and their shell may
/// hand the session its outside effects (`session.*`) without asking each time, as their
/// chosen shell. The grant is theirs, in the one policy every API decides with.
fn shell_access(
    app: &str,
    person: &str,
) -> impl Fn() -> Option<tilo_action_policy::MemberAccess> + 'static {
    use tilo_action_policy::{
        Answer, Grant, Grantee, KindSet, MemberAccess, TreePath, WorkspacePolicy,
    };
    let mut policy = WorkspacePolicy::new(SHELL_ROOM, person);
    policy.grants.push(Grant::new(
        format!("shell:{app}:session"),
        Grantee::person(person),
        TreePath::workspace(SHELL_ROOM).app(app),
        KindSet::OUTSIDE,
        Answer::Allow,
        Grantee::person(person),
    ));
    let access = MemberAccess::person(policy, person);
    move || Some(access.clone())
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
    /// Effects the shell handed the session's API, and those the session ran.
    pub handed: u64,
    pub served: u64,
}

/// One part of a tilolang shell on the desktop. See the module docs.
pub struct ShellPart {
    widgets: WidgetSurface,
    instance: Instance,
    /// What moved in the shell's store since the part last drew it: a write of another part
    /// (every display draws the same store, so a change any part writes is the whole shell's),
    /// or of a connection the session admits (a REPL, an agent).
    moved: Watch,
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
    /// part stands nowhere, draws nothing, takes no pointer and offers nothing on the API,
    /// until it is told a display again.
    gone: Option<String>,
    /// The outputs a fullscreen window covers, by connector name: the part on one of them
    /// steps aside, as the desktop's own bar does there, and leaves the display to the window.
    fullscreen: Vec<String>,
    /// A menu the part opened, or a surface of the shell's (its overview, the switcher, the
    /// launcher, the notifications), is up: the part stands over its output even where a
    /// fullscreen window owns it, so what the person opened shows.
    cover: bool,
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
    /// The wells the shell's views report into, as the session handed them: each part installs
    /// its own view of the one handle, keyed by the display it draws on, so two parts drawing
    /// one strip's well do not race for it.
    wells: Option<ui::widget::Wells>,
    /// The display this part draws on, as its views read it and its wells are keyed.
    screen: Option<tilo_session_core::shell::LayoutDisplayReading>,
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
    /// The link the session publishes its readings on and the shell asks through.
    pub link: Rc<SessionLink>,
    /// The shell's store, shared by every part of the shell.
    pub store: ResourceStore,
    /// The first part of a shell makes its roots and seeds them; every other part finds them
    /// made, so one shell has one seed, however many displays draw it.
    pub seed: bool,
}

impl ShellPart {
    /// The root view of `identity.app` (the last of its packages, which come in load order) for
    /// its person, across the part's output at `scale`, kept in step with the session through
    /// its link; `waker` wakes the session's loop when work the instance waits for is ready.
    ///
    /// The views' wells ([`ui::widget::Wells`]) report to `wells`, the handle the window policy
    /// placing the windows they stand for shares (`crate::platform::surfaces::WellPolicy`).
    ///
    /// The part is the shell's whole screen ([`screen_layer`]): it shows the person's
    /// layout and puts each window in its well.
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
            store,
            seed,
        } = identity;
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale,
        };
        // What a screen reader calls the shell's window: the name the view it fills declares
        // (`window "Desktop shell"`), or the shell's own when the view names none.
        let window_name = packages
            .iter()
            .rev()
            .find(|package| package.meta.app == app)
            .and_then(|package| {
                package
                    .meta
                    .views
                    .iter()
                    .find(|view| view.name == "shell_view")
            })
            .and_then(|view| view.window.clone())
            .unwrap_or_else(|| "Desktop shell".to_owned());
        tilo_app_window::i18n::ensure();
        let (app_end, window_end) = pair();
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: person.into(),
                mode: Mode::Live,
                device: None,
                store: store.clone(),
                sink: None,
                // The shell's whole screen, not whatever root the package lists first.
                root: screen_layer(),
                link_home: None,
            },
            Rc::new(OnSession {
                loads: ShellPlatform {
                    packages: Rc::new(packages),
                    link: Rc::clone(&link),
                },
                waker,
            }),
            Rc::new(tilolang_tilo::names::Installed),
            Box::new(app_end),
        );
        instance.set_access(Rc::new(shell_access(app, person)));
        let origin = Rc::new(RefCell::new(SessionOrigin::new(link)));
        instance.add_origin(Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin);
        instance.sync();
        let window = AppWindow::part(Box::new(window_end));
        // The whole screen lies under the windows, which stand in its wells; the bar rides over
        // them.
        let mut widgets = WidgetSurface::new(Box::new(window), placement, true).named(window_name);
        widgets.install_wells(wells.clone());
        let mut part = Self {
            widgets,
            instance,
            moved: store.watch(),
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
            previews: None,
            deadline: None,
            content: None,
            content_workspace: String::new(),
            wells,
            screen: None,
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
        // What a composed view opens is a place on the canvas, never a level of the shell's own
        // window.
        part.instance.place_opens(true);
        part
    }

    /// Make the shell's own roots (`root shell: Shell`, `root viewing: Viewing`) and let the
    /// package's migration seed them — the bar's fields and the dock's items, on a shell never
    /// arranged — as an app's migration seeds its own, written to the shell's store.
    fn seed(&mut self) {
        let Some(host) = self.instance.host() else {
            return;
        };
        let roots = host
            .package(self.instance.app())
            .map(|package| package.meta.named_roots)
            .unwrap_or_default();
        for root in &roots {
            if let Err(error) = host.root_resource(self.instance.app(), &root.ty) {
                tracing::warn!(%error, "the shell's roots could not be made");
                return;
            }
        }
        if roots.is_empty() {
            return;
        }
        // The roots' making first, then the seed that runs once they are had.
        for _ in 0..3 {
            self.instance.sync();
            if !self.commit() {
                break;
            }
        }
        // What the seed wrote is drawn: the part starts from it, not from a move.
        let _ = self.moved.take();
    }

    /// Stand on this part's whole output: the shell draws the whole screen, its bar and dock
    /// over the places the windows stand in.
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
    /// or while a surface of the shell's is (the overview, the switcher, the launcher, the
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
        let surface = !matches!(self.origin.borrow().link.surface().as_str(), "" | "none");
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
        self.widgets.control_bounds(name)
    }

    /// Where every control assistive technology names `name` stands: a menu item repeats the
    /// name of the row it moves a place to, which a chip of that row elsewhere says too.
    pub fn control_bounds_all(&mut self, name: &str) -> Vec<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        let ids: std::collections::HashSet<_> = self
            .widgets
            .host_mut()
            .a11y_nodes_named(name)
            .into_iter()
            .collect();
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter(|(id, _)| ids.contains(id))
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

    /// What this part's layout said went wrong since this was last asked, once per failure: a
    /// frame whose layout failed and was skipped, or a part that did not settle and was drawn
    /// as it last stood.
    pub fn take_layout_report(&mut self) -> Option<String> {
        self.widgets.host_mut().take_skip_report()
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
    /// desktop's logical layout `(x, y, width, height)`. A name more than one thing on the shell
    /// carries — a strip's name, which the bar's pill and nothing on the shell says — is read by all
    /// of them, so what a test asks of a name is what the whole shell says of it.
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
    /// the bar and every surface over the shell in it.
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

    /// What the part shows as a host that writes it as semantic HTML reads it
    /// (`ui::composition`): the bar, the strips and their windows as elements,
    /// with the words each shows, for a page that shows the shell instead of
    /// drawing it.
    #[must_use]
    pub fn composition(&self) -> ui::composition::Composition {
        self.widgets.composition()
    }

    /// The scene the part's last drawn frame encoded: what a display with no working GPU
    /// replays on the CPU (`tilo_cpu_raster`).
    #[must_use]
    pub fn scene(&self) -> &vello::Scene {
        self.widgets.scene()
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

    /// Tell the part the display it draws on, as its views read it (`device.screen`): its id,
    /// name, rectangle, scale and whether it is primary. The part's wells are keyed by the same
    /// id, so a strip two parts draw cannot place its window twice. `None`: the display went.
    pub fn set_screen(&mut self, display: Option<tilo_session_core::shell::LayoutDisplayReading>) {
        if self.screen == display {
            return;
        }
        let id = display.as_ref().map(|display| display.id);
        if display.is_none()
            && let Some(wells) = &self.wells
            && let Some(gone) = &self.screen
        {
            // The display went: what its part laid out stands nowhere now, so the policy places
            // no window there.
            wells.for_part(gone.id.to_string()).clear();
        }
        self.screen = display.clone();
        self.widgets.install_wells(
            self.wells
                .clone()
                .map(|wells| wells.for_part(id.map(|id| id.to_string()).unwrap_or_default())),
        );
        let reading = display.map(|display| {
            serde_json::json!({
                "output": display.id,
                "connector": display.connector,
                "name": display.name,
                "x": display.x,
                "y": display.y,
                "width": display.width,
                "height": display.height,
                "scale": display.scale,
                "primary": display.primary,
                "identify": display.identify,
                "identify_number": display.identify_number,
                "identify_name": display.identify_name,
            })
        });
        self.origin.borrow_mut().set_screen(reading);
        // What it draws is another display's now: the next frame draws it again.
        self.again = true;
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
    pub fn widgets_mut(&mut self) -> &mut WidgetSurface {
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

    /// Write what the instance planned to the shell's store, and bring the instance up to date
    /// with it. Whether anything was written.
    fn commit(&mut self) -> bool {
        self.instance.commit_planned();
        if self.moved.peek().is_empty() {
            return false;
        }
        let _ = self.moved.take();
        let at = std::time::Instant::now();
        self.instance.sync();
        self.work.syncing = self.work.syncing.saturating_add(at.elapsed());
        true
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
        // A change another display's part of this shell wrote reaches this one: the store it
        // shares moved, and it draws the shell as it is now. Nothing else changed, so no other
        // frame is owed.
        if self.record_moved() {
            let _ = self.moved.take();
            self.instance.sync();
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
        // session arranges on the canvas, one step of the person's, never a detail level of the
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
        self.input || self.again || self.owes() || self.origin.borrow().due() || self.record_moved()
    }

    /// Whether the shell's store holds changes this part has not drawn: another part of the
    /// shell wrote them, or a connection the session admits (a REPL, an agent) changed the
    /// resources the shell's own actions are published on, and the part draws them next frame.
    fn record_moved(&self) -> bool {
        !self.moved.peek().is_empty()
    }
}

impl tilo_session_core::surfaces::Published for ShellPart {
    /// A part whose display went offers nothing: the parts still there answer the API.
    fn offered(&self) -> bool {
        !self.retired()
    }

    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        tilo_session_core::surfaces::Published::resources(&mut self.widgets)
    }

    fn revision(&self) -> Option<u64> {
        tilo_session_core::surfaces::Published::revision(&self.widgets)
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError> {
        self.input = true;
        tilo_session_core::surfaces::Published::invoke(&mut self.widgets, address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        tilo_session_core::surfaces::Published::accessible_labels(&mut self.widgets)
    }

    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        tilo_session_core::surfaces::Published::controls(&mut self.widgets)
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        // A press takes the part into its turn, as a press of the pointer does: a named press
        // (an assistive technology's activation, `tilo-control click`) changes the shell's
        // record, and the change is drawn by the turn that takes it in. The part wakes for
        // input alone, so a press that did not say so waited for the next deadline — the
        // clock — the shell drawing the style the person left until then.
        self.input = true;
        tilo_session_core::surfaces::Published::press(&mut self.widgets, name)
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        tilo_session_core::surfaces::Published::a11y_tree(&mut self.widgets)
    }

    fn a11y_name(&self) -> Option<String> {
        tilo_session_core::surfaces::Published::a11y_name(&self.widgets)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        self.input = true;
        tilo_session_core::surfaces::Published::a11y_action(&mut self.widgets, target, action)
    }

    fn wants_keyboard(&self) -> bool {
        ShellSurface::wants_keyboard(self)
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
enum SwitcherKey {
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
    /// What `key` means to the shell while a surface of the shell's is up, or as a press that
    /// opens one. `None` when it belongs to whatever has the keyboard.
    fn switcher_key(&self, key: &SurfaceKey) -> Option<SwitcherKey> {
        if key.pressed && key.modifiers.alt {
            let app_scope = XKB_GRAVE.contains(&key.keysym);
            if key.keysym == XKB_TAB || app_scope {
                return Some(SwitcherKey::Walk {
                    back: key.modifiers.shift,
                    app_scope,
                });
            }
        }
        let up = self.origin.borrow().link.surface();
        if matches!(up.as_str(), "" | "none") {
            return None;
        }
        if up == "switcher" && matches!(key.keysym, XKB_ALT_L | 0xffea) && !key.pressed {
            return Some(SwitcherKey::Commit);
        }
        match key.keysym {
            0xff0d if key.pressed && up == "switcher" => Some(SwitcherKey::Commit),
            0xff1b if key.pressed => Some(SwitcherKey::Close),
            _ => None,
        }
    }
}

impl ShellPart {
    /// Ask the session for the surface action `action` with `arguments`: what a key of the part's
    /// own asks, as a press of the views asks through the session's API.
    fn ask(&self, action: &str, arguments: &[(&str, serde_json::Value)]) {
        let arguments = arguments
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        self.origin.borrow().link.ask(Ask {
            resource: OVERLAYS.to_owned(),
            action: action.to_owned(),
            arguments,
            from: None,
        });
    }

    /// What a composed view opened since the last turn, each placed on the canvas through the
    /// layout's own `place`, one arranging step of the person's: a video row pressed on a
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
                resource: LAYOUT_RESOURCE.to_owned(),
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
        let before = tilo_session_core::surfaces::Published::revision(&self.widgets);
        let started = std::time::Instant::now();
        let drew = self.widgets.prepare_frame();
        let changed = tilo_session_core::surfaces::Published::revision(&self.widgets) != before;
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
mod places_tests;

#[cfg(test)]
mod switcher_keys;
