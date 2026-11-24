//! An app's window with nothing on screen: its instance over a workspace of its own, staged with
//! one of its scenarios, sending its views over the patch protocol to the app window's half of the
//! window ([`tilo_app_window::AppWindow`]), which the toolkit's frame pipeline lays out
//! ([`ui::headless::FrameHarness`]). Nothing is rasterised and no GPU is opened.
//!
//! `tilo-layout-check` lays these windows out to check them, and a browser page
//! (`tilolang-web`) writes them as HTML: one way to open a window, one exchange between it and its
//! instance, one platform ([`Packages`]) under both.

use std::rc::Rc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;
use tilolang_tilo::runtime::{Actor, InvokeOutcome, Value};
use ui::headless::FrameHarness;

mod platform;

pub use platform::Packages;

/// How often the window and its instance may answer each other before a frame is taken as it is:
/// the rows a list shows, then the rows those rows show.
pub const EXCHANGES: usize = 8;

/// A file a window opens with, as a page hands one over: its name, the media type it declares,
/// and bytes this host keeps as the content the app's action reads.
pub struct OpenedFile<'a> {
    pub name: &'a str,
    pub media: &'a str,
    pub bytes: &'a [u8],
}

/// How a window opens, beyond its app and its packages.
pub struct Opening<'a> {
    /// The layer a package with more than one root shows (`Shell`, `Layout`), and its view.
    pub root: tilolang_tilo::host::RootView,
    /// The scenario that fills its workspace first; `None` opens it empty, as a first launch does.
    pub scenario: Option<&'a str>,
    /// The person it opens as: their member id and the name they go by.
    pub me: (&'a str, &'a str),
    /// The size it is laid out at, in logical pixels, and its theme.
    pub viewport: (u32, u32),
    pub theme: ui::Theme,
    /// Where the workspace's resources open on the web, when a page stands behind the window:
    /// what a resource's `share-link` mints under ([`tilolang_tilo::share`]).
    pub link_home: Option<tilolang_tilo::share::LinkHome>,
    /// How many items the workspace holds when the window opens ([`tilolang_tilo::seed`]).
    pub seed: usize,
    /// Whether what the app's units run is counted ([`tilolang_tilo::runtime::cost`]).
    pub costs: bool,
    /// The view states it stands in before anyone picks, as the app's own code names them
    /// (`place=Task.today`).
    pub states: &'a [String],
    /// The file the app is opened with, which its package's action for that kind reads as the
    /// same `File` a chooser gives it. What the action made opens in the window.
    pub open: Option<OpenedFile<'a>>,
    /// The link the app is opened with, which its `open-link` opens as a shell hands one over.
    pub link: Option<&'a str>,
}

impl Opening<'_> {
    /// A window of the package's first root, opened as Ada, empty, at `viewport` in `theme`.
    #[must_use]
    pub fn new(viewport: (u32, u32), theme: ui::Theme) -> Self {
        Self {
            root: tilolang_tilo::host::RootView {
                root: None,
                view: None,
            },
            scenario: None,
            me: ("ada", "Ada"),
            viewport,
            theme,
            link_home: None,
            seed: 0,
            costs: false,
            states: &[],
            open: None,
            link: None,
        }
    }
}

/// One window of an app and the instance that runs it.
pub struct Window {
    pub instance: Instance,
    pub harness: FrameHarness,
}

impl Window {
    /// The app `app` of `packages` (the app last), opened as `opening` says.
    ///
    /// # Errors
    /// The app declares no root to open a window on, a root the package declares could not be
    /// made, the workspace could not be seeded, a view state names nothing, the scenario failed,
    /// or the window refused what the app sent.
    pub fn open(app: &str, packages: &Rc<Packages>, opening: Opening<'_>) -> Result<Self, String> {
        // A window opens on an app's root resource. A package with none publishes views a host
        // draws itself (a player's card in the shell) and has no window to open.
        if packages
            .compiled
            .iter()
            .map(|package| &package.meta)
            .find(|meta| meta.app == app)
            .is_some_and(|meta| meta.roots.is_empty())
        {
            return Err(format!(
                "{app} declares no root, so it opens no window: its views are drawn where a host shows them"
            ));
        }
        let Opening {
            root,
            scenario,
            me,
            viewport,
            mut theme,
            link_home,
            seed,
            costs,
            states,
            open,
            link,
        } = opening;
        theme.reduce_motion = true;
        let (app_end, window_end) = pair();
        // The person the window opens as is called by their name, and everyone else as a window
        // calls them when nobody told it their names: a scenario's sample person (`person("Leo")`)
        // by that name, never by their id.
        let names = Rc::new(tilolang_tilo::names::Told::new(
            me.0.into(),
            Rc::new(tilolang_tilo::names::Installed),
        ));
        names.tell(me.1.to_owned(), Vec::new(), Vec::new(), Vec::new());
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: me.0.into(),
                mode: Mode::Live,
                device: None,
                store: tilolang_tilo::resources::ResourceStore::in_memory(),
                root,
                sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
                link_home,
            },
            Rc::clone(packages) as Rc<dyn Platform>,
            names,
            Box::new(app_end),
        );
        instance.sync();
        // No frame comes to take in the indexes its load builds off the frame, so they are waited
        // for, as a host without frames waits for them.
        instance.settle_pending();
        // A window opens on the app's root view: one that did not run leaves the window empty, so
        // the host that laid it out is told why rather than showing nothing.
        if let Some(why) = instance.take_open_failure() {
            return Err(format!("the app's window did not open: {why}"));
        }
        // Counted from the first frame: a workspace's opening is what it costs to open.
        if costs && let Some(host) = instance.host() {
            host.runtime().record_costs(true);
        }
        make_roots(&instance, app)?;
        if seed > 0
            && let Some(host) = instance.host()
        {
            tilolang_tilo::seed::seed(&host, app, seed, &packages.now())?;
        }
        instance.commit_planned();
        if let Some(host) = instance.host() {
            for state in states {
                let (name, text) = state.split_once('=').ok_or_else(|| {
                    format!("--state {state} names no value: write `--state place=Task.today`")
                })?;
                host.runtime()
                    .stage_view_state(app, name.trim(), text.trim())
                    .map_err(|error| format!("--state {state}: {error}"))?;
            }
        }
        instance.stage_story(scenario);
        // The scenario runs on the frame that takes it: its sample data is what it writes
        // without a time of its own, and what it dates is the story it tells.
        if scenario.is_some() {
            instance.sync();
            instance.commit_planned();
        }
        if let Some(why) = instance.take_stage_failure() {
            return Err(format!(
                "the scenario \"{}\" failed: {why}",
                scenario.unwrap_or_default()
            ));
        }
        // What the page opened the app with, run as a shell launches the app on it: the app's
        // own action for the file's kind, or its `open-link`, and what either made opens.
        if let Some(opened) = open {
            opened_file(&mut instance, app, packages, me.0, opened)?;
        }
        if let Some(link) = link {
            opened_link(&mut instance, app, me.0, link)?;
        }
        instance.update();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        window.receive();
        if let Some(why) = window.failed() {
            return Err(format!("the window refused what the app sent: {why}"));
        }
        let harness =
            FrameHarness::new_with_theme(ui::State::new(), Box::new(window), viewport, theme);
        Ok(Self { instance, harness })
    }

    /// The app window's half of the window.
    pub fn app_window(&mut self) -> Option<&mut AppWindow> {
        self.harness
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Let the window and its instance answer each other until neither has more to say, laying the
    /// window out after each answer.
    pub fn exchange(&mut self) {
        for _ in 0..EXCHANGES {
            self.instance.settle_pending();
            if let Some(window) = self.app_window() {
                window.flush();
            }
            let moved = self.instance.update();
            let received = self.app_window().is_some_and(AppWindow::receive);
            let owing =
                self.app_window().is_some_and(|window| window.owes()) || self.instance.pending();
            if !moved && !received && !owing {
                break;
            }
            self.harness.relayout();
        }
    }
}

/// Run the action `app`'s package declares for `opened`'s kind, on the root it opens into, with
/// the same `File` a chooser gives it: the page keeps the bytes in its own content store, and
/// what the action made opens in the window. One path for a page and a device, which the native
/// host runs from the launch it was started with.
fn opened_file(
    instance: &mut Instance,
    app: &str,
    packages: &Packages,
    person: &str,
    opened: OpenedFile<'_>,
) -> Result<(), String> {
    let file = packages.keep_content(opened.name, opened.media, opened.bytes)?;
    let (action, root) = {
        let host = instance
            .host()
            .ok_or_else(|| "the app has not started".to_owned())?;
        let package = host
            .package(app)
            .ok_or_else(|| format!("`{app}` is not loaded"))?;
        let action = tilolang_tilo::meta::action_opening(
            &package.meta,
            Some(opened.name),
            opened.media,
        )
        .cloned()
        .ok_or_else(|| format!("`{app}` does not open `{}`", opened.name))?;
        let target = tilolang_tilo::meta::opening_root(&package.meta, &action)
            .ok_or_else(|| format!("`{app}` opens files into no root"))?;
        let root = host
            .root_resource(app, &target.id)
            .map_err(|error| error.to_string())?;
        (action.id, root)
    };
    invoke_opened(instance, app, person, &action, &root, Opened::File(file))
}

/// Run `app`'s `open-link` with `link`: the video, page or package a link names opens, as a
/// shell hands the link to the app.
fn opened_link(
    instance: &mut Instance,
    app: &str,
    person: &str,
    link: &str,
) -> Result<(), String> {
    let (action, root) = {
        let host = instance
            .host()
            .ok_or_else(|| "the app has not started".to_owned())?;
        let package = host
            .package(app)
            .ok_or_else(|| format!("`{app}` is not loaded"))?;
        let (target, action) = tilolang_tilo::meta::open_link(&package.meta)
            .ok_or_else(|| format!("`{app}` does not open links"))?;
        let root = host
            .root_resource(app, &target.id)
            .map_err(|error| error.to_string())?;
        (action.id.clone(), root)
    };
    invoke_opened(instance, app, person, &action, &root, Opened::Link(link))
}

/// What an app is opened with.
enum Opened<'a> {
    File(tilolang_tilo::runtime::blob::FileValue),
    Link(&'a str),
}

/// Invoke `action` on `root` with what the app is opened with, and open what it made in the
/// window.
fn invoke_opened(
    instance: &mut Instance,
    app: &str,
    person: &str,
    action: &str,
    root: &tilolang_tilo::runtime::ResourceRef,
    opened: Opened<'_>,
) -> Result<(), String> {
    let host = instance
        .host()
        .ok_or_else(|| "the app has not started".to_owned())?;
    let actor = Actor::Member(MemberId::person(person));
    let argument = match opened {
        Opened::File(file) => Value::file(file),
        Opened::Link(link) => Value::text(link),
    };
    match host
        .runtime()
        .invoke_action(&actor, (action, vec![Value::resource(root), argument]))
    {
        Ok(InvokeOutcome::Done(Value::Ref(made))) => {
            let made = Rc::unwrap_or_clone(made.target);
            instance.opens_what_it_made(tilolang_tilo::publish::resource_id(
                app,
                &made.id_string(),
            ));
            Ok(())
        }
        // An action that made no resource, and one that waits on another app or a person, which
        // answers nobody here.
        Ok(InvokeOutcome::Done(_) | InvokeOutcome::Asked(_)) => Ok(()),
        Ok(other) => Err(format!("the app was not opened: {other:?}")),
        Err(error) => Err(error.to_string()),
    }
}

/// The package's own root resources — every layer it has (the shell's `Shell` beside its layout)
/// and a person's record kept beside them (`root shell: Shell`) — made with what each starts
/// with, as the live session does when it loads the package: a scenario that seeds another layer
/// than the one laid out shows them.
fn make_roots(instance: &Instance, app: &str) -> Result<(), String> {
    let Some(host) = instance.host() else {
        return Ok(());
    };
    let meta = host.package(app).map(|package| package.meta);
    let roots = meta.iter().flat_map(|meta| {
        meta.roots
            .iter()
            .cloned()
            .chain(meta.named_roots.iter().map(|root| root.ty.clone()))
    });
    for ty in roots {
        host.root_resource(app, &ty)
            .map_err(|error| format!("a root could not be made: {error}"))?;
    }
    Ok(())
}
