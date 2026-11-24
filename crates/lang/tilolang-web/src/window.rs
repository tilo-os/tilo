//! An app's window as a web page: the window `tilo-layout-check` lays out
//! ([`tilolang_window::Window`]), staged with one of the app's scenarios, its laid-out frame
//! written as semantic HTML ([`ui::composition::page`]), and what the person does on that page
//! acted on the window as their click or their typing is.

use std::rc::Rc;

use tilolang_window::{OpenedFile, Opening, Packages, Window};
use ui::composition::page::{PAGE_CSS, Page, PageAction, Patch, Shown, theme_css};

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
    /// The file the page opened the app with, kept in the page's own content store and handed to
    /// the action the app declares for its kind.
    pub open: Option<OpenedFile<'a>>,
    /// The link the page opened the app with: the app's `open-link` shows what it names.
    pub link: Option<&'a str>,
}

/// One app's window, written as a page.
pub struct AppPage {
    window: Window,
    page: Page,
    theme: ui::Theme,
}

impl AppPage {
    /// The app `app` of `packages` (the app last), staged as `staging` says.
    ///
    /// # Errors
    /// A root the package declares could not be made, its scenario failed, or the window refused
    /// what the app sent.
    pub fn open(app: &str, packages: &Rc<Packages>, staging: Staging<'_>) -> Result<Self, String> {
        let mut theme = if staging.dark {
            ui::Theme::dark()
        } else {
            ui::Theme::light()
        };
        theme.reduce_motion = true;
        let mut opening = Opening::new(staging.viewport, theme.clone());
        opening.root.root = staging.root.map(str::to_owned);
        opening.scenario = staging.scenario;
        opening.link_home = staging.link_home;
        opening.open = staging.open;
        opening.link = staging.link;
        let mut window = Window::open(app, packages, opening)?;
        window.exchange();
        Ok(Self {
            window,
            page: Page::new(),
            theme,
        })
    }

    /// Whether what the page shows declares `protect capture`: a capture or a stream of the page
    /// leaves the view out.
    #[must_use]
    pub fn capture_protected(&self) -> bool {
        self.window.harness.capture_protected()
    }

    /// The page's stylesheet: the theme's colours and the page's own rules.
    #[must_use]
    pub fn css(&self) -> String {
        theme_css(&self.theme) + PAGE_CSS
    }

    /// The whole page body, numbering its elements afresh.
    pub fn render(&mut self) -> String {
        let composition = self.window.harness.composition();
        self.page.render(&composition)
    }

    /// What of the window the page last wrote shows other than as the browser's own elements
    /// ([`Page::shown`]).
    #[must_use]
    pub fn gaps(&self) -> &Shown {
        self.page.shown()
    }

    /// The elements that changed since the page was last written, each replacing the element with
    /// its `data-p`.
    pub fn patch(&mut self) -> Vec<Patch> {
        let composition = self.window.harness.composition();
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
        action: PageAction,
    ) -> Result<Vec<Patch>, String> {
        if let PageAction::Resize { width, height } = action {
            return self.resize((width, height));
        }
        self.page
            .act(&mut self.window.harness, id, target, action)?;
        self.window.exchange();
        Ok(self.patch())
    }

    /// The link to the resource `resource` of this page's app, minted under the workspace's page
    /// the page was opened with ([`tilolang_tilo::instance::Instance::share_link`]).
    ///
    /// # Errors
    /// Why no link can be made: no workspace page stands behind this window, or the resource is
    /// not the app's.
    pub fn share_link(&self, resource: &str) -> Result<serde_json::Value, String> {
        self.window
            .instance
            .share_link(resource)
            .map(|output| output.value.unwrap_or(serde_json::Value::Null))
    }

    /// The window laid out at a new size: a narrow one shows one column of a split at a time.
    ///
    /// # Errors
    /// The window could not be laid out at that size.
    pub fn resize(&mut self, viewport: (u32, u32)) -> Result<Vec<Patch>, String> {
        self.window
            .harness
            .resize(viewport)
            .map_err(|error| format!("layout failed: {error:?}"))?;
        self.window.exchange();
        Ok(self.patch())
    }
}
