//! An app's window as a web page: its instance over a workspace of its own, staged with one of its
//! scenarios, sending its views to the app window's half of the window, which the toolkit's frame
//! pipeline lays out; the laid-out frame is written as semantic HTML
//! ([`ui::composition::page`]), and what the person does on that page is acted on the window as
//! their click or their typing is.
//!
//! This is the window `tilolang layout-check` lays out, and the page a server renders for a
//! browser: one composition, one HTML writer, one set of semantic actions.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::resources::{LocalPort, ResourceStore};
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use ui::composition::page::{PAGE_CSS, Page, Patch, theme_css};
use ui::headless::FrameHarness;

use crate::Packages;

/// The person a page's window opens as, and the name it calls them by.
const ME: &str = "ada";
const MY_NAME: &str = "Ada";

/// How often the window and its instance may answer each other before a page is written as it
/// is: the rows a list shows, then the rows those rows show.
const EXCHANGES: usize = 8;

/// How a page's window is staged.
pub struct Staging<'a> {
    /// The scenario that fills its workspace first; `None` opens it empty, as a first launch does.
    pub scenario: Option<&'a str>,
    /// The layer a package with more than one root shows (`Shell`, `Layout`).
    pub root: Option<&'a str>,
    /// The size the window is laid out at, in logical pixels.
    pub viewport: (u32, u32),
    pub dark: bool,
    /// Where the workspace's resources open on the web, when the page stands behind one: what a
    /// resource's `share-link` mints under ([`tilolang_tilo::share`]).
    pub link_home: Option<tilolang_tilo::share::LinkHome>,
}

/// What a page shows other than as the browser's own elements ([`AppPage::gaps`]).
#[derive(Debug, Default, serde::Serialize)]
pub struct Gaps {
    pub painted: BTreeMap<String, usize>,
    pub placed: BTreeMap<String, usize>,
}

/// One app's window, written as a page.
pub struct AppPage {
    instance: Instance,
    harness: FrameHarness,
    page: Page,
    theme: ui::Theme,
    /// What the view asked of apps the page does not run, for its host to carry out.
    asked: Rc<RefCell<Vec<Invocation>>>,
}

/// What a hosted page's view asks of apps it does not run (a window's Close asks the session to
/// close its place): kept for the page's host, which carries each out where those apps' state is.
struct Asked(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Asked {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Sent
    }
}

/// How a host shows an app's view of the workspace's own state on a page ([`AppPage::hosted`]).
pub struct Hosting<'a> {
    /// The root the view is of (`layout`) and the view (`frames`).
    pub root: &'a str,
    pub view: &'a str,
    /// Where the resources the view reads come from: the workspace's own state, as a native host
    /// writes it.
    pub origin: tilolang_tilo::origin::SharedOrigin,
    /// The person the page is shown to: their member id and the name they go by.
    pub me: (&'a str, &'a str),
    pub viewport: (u32, u32),
    pub dark: bool,
    /// Where the workspace's resources open on the web, when the page is a workspace's: what a
    /// resource's `share-link` mints under ([`tilolang_tilo::share`]).
    pub link_home: Option<tilolang_tilo::share::LinkHome>,
}

/// How a page's instance starts: from a scenario, or over a host's origin.
struct Start<'a> {
    root: tilolang_tilo::host::RootView,
    scenario: Option<&'a str>,
    origin: Option<tilolang_tilo::origin::SharedOrigin>,
    me: (&'a str, &'a str),
    viewport: (u32, u32),
    dark: bool,
    link_home: Option<tilolang_tilo::share::LinkHome>,
}

impl AppPage {
    /// The app `app` of `packages` (the app last), staged as `staging` says.
    ///
    /// # Errors
    /// A root the package declares could not be made, its scenario failed, or the window refused
    /// what the app sent.
    pub fn open(app: &str, packages: &Rc<Packages>, staging: &Staging<'_>) -> Result<Self, String> {
        Self::start(
            app,
            packages,
            Start {
                root: tilolang_tilo::host::RootView {
                    root: staging.root.map(str::to_owned),
                    view: None,
                },
                scenario: staging.scenario,
                origin: None,
                me: (ME, MY_NAME),
                viewport: staging.viewport,
                dark: staging.dark,
                link_home: staging.link_home.clone(),
            },
        )
    }

    /// The app `app` of `packages` (the app last) as a host shows it on a page: the view
    /// `hosting.view` of its root `hosting.root`, over what `hosting.origin` writes there, as a
    /// native host shows it. Nothing is staged: the page shows the workspace's own state.
    ///
    /// # Errors
    /// A root the package declares could not be made, or the window refused what the app sent.
    pub fn hosted(
        app: &str,
        packages: &Rc<Packages>,
        hosting: Hosting<'_>,
    ) -> Result<Self, String> {
        Self::start(
            app,
            packages,
            Start {
                root: tilolang_tilo::host::RootView {
                    root: Some(hosting.root.to_owned()),
                    view: Some(hosting.view.to_owned()),
                },
                scenario: None,
                origin: Some(hosting.origin),
                me: hosting.me,
                viewport: hosting.viewport,
                dark: hosting.dark,
                link_home: hosting.link_home,
            },
        )
    }

    fn start(app: &str, packages: &Rc<Packages>, start: Start<'_>) -> Result<Self, String> {
        let mut theme = if start.dark {
            ui::Theme::dark()
        } else {
            ui::Theme::light()
        };
        theme.reduce_motion = true;
        let (app_end, window_end) = pair();
        let names = Rc::new(tilolang_tilo::names::Told::new(
            start.me.0.into(),
            Rc::new(tilolang_tilo::names::Installed),
        ));
        names.tell(start.me.1.to_owned(), Vec::new(), Vec::new());
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: start.me.0.into(),
                mode: Mode::Live,
                device: None,
                store: ResourceStore::new(Rc::new(LocalPort::in_memory())),
                root: start.root,
                sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
                link_home: start.link_home,
            },
            Rc::clone(packages) as Rc<dyn Platform>,
            names,
            Box::new(app_end),
        );
        let hosted = start.origin.is_some();
        if let Some(origin) = start.origin {
            instance.add_origin(origin);
        }
        instance.sync();
        instance.settle_pending();
        // The package's own roots, every layer it has, are made with what each starts with, as a
        // session makes them when it loads the package.
        let asked = Rc::new(RefCell::new(Vec::new()));
        if let Some(host) = instance.host() {
            // A hosted view's calls to the apps whose state the host keeps (the session's
            // arranging actions on the layout) are the host's to carry out.
            if hosted {
                host.runtime().set_foreign_invoker(Asked(Rc::clone(&asked)));
            }
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
        }
        instance.stage(start.scenario);
        // The scenario runs with the instance's next frame.
        instance.sync();
        if let Some(why) = instance.take_stage_failure() {
            return Err(format!(
                "the scenario \"{}\" failed: {why}",
                start.scenario.unwrap_or_default()
            ));
        }
        instance.update();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        window.receive();
        if let Some(why) = window.failed() {
            return Err(format!("the window refused what the app sent: {why}"));
        }
        let harness = FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(window),
            start.viewport,
            theme.clone(),
        );
        let mut page = Self {
            instance,
            harness,
            page: Page::new(),
            theme,
            asked,
        };
        page.exchange();
        Ok(page)
    }

    /// What the view asked of apps the page does not run since this was last called, oldest
    /// first: the host carries each out, and what that changes comes back as its origin's next
    /// reading ([`Self::follow`]).
    pub fn take_asked(&mut self) -> Vec<Invocation> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// What the host shows in each of the page's wells, by place ([`Page::wells`]): what stands
    /// where a native host shows that place's window.
    pub fn fill_wells(&mut self, wells: std::collections::HashMap<String, String>) {
        self.page.wells = wells;
    }

    /// Hand the page's views the reading of `published`, a resource the host publishes for its
    /// shell (`session:apps`, `session:windows`, `desktop:overlays`): what a device's shell reads
    /// of its own state, so a page's shell reads the same names and needs no branch for the web.
    /// The page's next render shows what the reading says.
    ///
    /// # Errors
    /// `published` is no resource of the platform's packages, or its reading is not the object of
    /// fields they describe.
    pub fn fill_reading(
        &mut self,
        published: &str,
        reading: &serde_json::Value,
    ) -> Result<(), String> {
        let ty = tilolang_tilo::platform_apps::platform_roots()
            .into_iter()
            .find(|(id, _)| *id == published)
            .map(|(_, ty)| ty)
            .ok_or_else(|| format!("`{published}` is no resource the platform publishes"))?;
        let target = tilolang_tilo::runtime::ResourceRef::parse_id(published, ty)
            .ok_or_else(|| format!("`{published}` names no resource"))?;
        let host = self
            .instance
            .host()
            .ok_or_else(|| "the page's host is not open".to_owned())?;
        host.runtime()
            .set_reading(&target, &reading.to_string())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// What changed on the page since it was last written, after the instance took in what its
    /// origins hold now (another member moved a place): the elements to replace.
    pub fn follow(&mut self) -> Vec<Patch> {
        self.instance.sync();
        self.exchange();
        self.patch()
    }

    fn window(&mut self) -> Option<&mut AppWindow> {
        self.harness
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Let the window and its instance answer each other until neither has more to say, laying the
    /// window out after each answer.
    fn exchange(&mut self) {
        for _ in 0..EXCHANGES {
            self.instance.settle_pending();
            if let Some(window) = self.window() {
                window.flush();
            }
            let moved = self.instance.update();
            let received = self.window().is_some_and(AppWindow::receive);
            let owing =
                self.window().is_some_and(|window| window.owes()) || self.instance.pending();
            self.harness.relayout();
            if !moved && !received && !owing {
                break;
            }
        }
    }

    /// The page's stylesheet: the theme's colours and the page's own rules.
    #[must_use]
    pub fn css(&self) -> String {
        theme_css(&self.theme) + PAGE_CSS
    }

    /// The whole page body, numbering its elements afresh.
    pub fn render(&mut self) -> String {
        let composition = self.harness.composition();
        self.page.render(&composition)
    }

    /// What of the window the page cannot show as the browser's own elements, by widget, with how
    /// many of each it holds: `painted`, widgets that show as a picture of their paint, which
    /// nothing paints in a page here; `placed`, containers that place their children at
    /// rectangles of their own, which the page places at those rectangles.
    #[must_use]
    pub fn gaps(&self) -> Gaps {
        let composition = self.harness.composition();
        let mut gaps = Gaps::default();
        let mut stack: Vec<&ui::composition::Composed> = composition.layers.iter().collect();
        stack.push(&composition.root);
        while let Some(node) = stack.pop() {
            let count = |kind: &mut BTreeMap<String, usize>| {
                let held = kind.entry(node.widget.clone()).or_default();
                *held = held.saturating_add(1);
            };
            if node.picture.is_some() {
                count(&mut gaps.painted);
            }
            if !node.flows && !node.children.is_empty() {
                count(&mut gaps.placed);
            }
            stack.extend(node.children.iter());
        }
        gaps
    }

    /// The elements that changed since the page was last written, each replacing the element with
    /// its `data-p`.
    pub fn patch(&mut self) -> Vec<Patch> {
        let composition = self.harness.composition();
        self.page.patch(&composition)
    }

    /// What the person did on the element numbered `id` (its `data-p`), at the semantic `target`
    /// it carries (its `data-t`), acted on the window; then what the page shows of it.
    ///
    /// # Errors
    /// The page numbers no such element, or the window refuses the action.
    pub fn act(
        &mut self,
        id: u32,
        target: Option<&str>,
        action: &ui::a11y::Action,
    ) -> Result<Vec<Patch>, String> {
        let path = self
            .page
            .path(id)
            .ok_or_else(|| format!("the page numbers no element {id}"))?;
        // An element a page numbers for a part of a widget (a document's block, a control) is
        // acted on through the widget it is a part of.
        let widget = path
            .split_once('#')
            .map_or(path, |(widget, _)| widget)
            .to_owned();
        self.harness
            .perform_semantic_named(&widget, target, action)?;
        self.exchange();
        Ok(self.patch())
    }

    /// The link to the resource `resource` of this page's app, minted under the workspace's page
    /// the page was opened with ([`Instance::share_link`]).
    ///
    /// # Errors
    /// Why no link can be made: no workspace page stands behind this window, or the resource is
    /// not the app's.
    pub fn share_link(&self, resource: &str) -> Result<serde_json::Value, String> {
        self.instance
            .share_link(resource)
            .map(|output| output.value.unwrap_or(serde_json::Value::Null))
    }

    /// The window laid out at a new size: a narrow one shows one column of a split at a time.
    ///
    /// # Errors
    /// The window could not be laid out at that size.
    pub fn resize(&mut self, viewport: (u32, u32)) -> Result<Vec<Patch>, String> {
        self.harness
            .resize(viewport)
            .map_err(|error| format!("layout failed: {error:?}"))?;
        self.exchange();
        Ok(self.patch())
    }
}
