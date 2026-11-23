//! A hosted window shown on a web page: the window of an instance a tilolang host runs, laid out
//! headlessly where nobody draws it, and answered as the page's markup
//! (`docs/architecture/html-renderer.md`).
//!
//! A page is a view like any window shown elsewhere. Its instance runs in the app's process and
//! sends this window component patches over its patch connection ([`HostedWindow::over`]). The
//! page is written from the window's semantics tree ([`Page`]), and what the person does on the
//! page comes back as the node's own semantic action, on the path a native window's
//! accessibility and automation take. Nothing here holds the app's data: what the person changes
//! is the app's own action, run by its instance under the identity its connection was opened as.

use tilo_render_protocol::channel::Channel;
use tilo_server_protocol::page::window::{self, WindowAnswer, WindowAsk};
use tilo_server_protocol::page::{PageAction, PagePatch};
use ui::a11y::Action;
use ui::composition::Composition;
use ui::composition::page::{PAGE_CSS, Page, Patch, theme_css};
use ui::headless::FrameHarness;

use super::hosted::HostedWindow;

/// The size a page's view is laid out at until the page says its own ([`PageAction::Resize`]):
/// the wide layout, with its sidebar and page side by side.
pub const VIEWPORT: (u32, u32) = (1280, 800);

/// The smallest and largest side, in CSS pixels, a page's size lays its view out at.
const PAGE_SIDE: (u32, u32) = (240, 8192);

/// The most frames one read of the page lays out to take up what its view owes: a view that keeps
/// asking for frames (an animation) is not waited out.
const PAGE_FRAMES: usize = 8;

/// The page whole, as a browser loads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The window's name: the item its view shows, else what its shell's headers name.
    pub title: String,
    /// The theme's stylesheet and the page's own.
    pub css: String,
    pub body: String,
}

/// The frame a page lays out `window` in, where nobody draws it.
#[must_use]
pub fn frame_of(window: HostedWindow) -> FrameHarness {
    FrameHarness::new(ui::State::new(), Box::new(window), VIEWPORT)
}

/// The frame a page lays out the window over the patch connection `channel` in, whose other end
/// its instance's host holds.
#[must_use]
pub fn frame_over(channel: Channel) -> FrameHarness {
    frame_of(HostedWindow::over(channel))
}

/// A window's frame shown on one page: the numbers its elements carry from one answer to the
/// next. See the module docs.
#[derive(Default)]
pub struct ShownOnPage {
    page: Page,
}

impl ShownOnPage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The page whole of `frame`, after activating `show` where the page's address names
    /// something the view shows (an item of its list). An address that names nothing it shows
    /// opens the view as it stands.
    pub fn open(&mut self, frame: &mut FrameHarness, show: Option<&str>) -> Shown {
        if let Some(show) = show {
            let composition = composition(frame);
            let opened = widget_of_target(&composition, show)
                .ok_or_else(|| "nothing the view shows".to_owned())
                .and_then(|widget| {
                    frame.perform_semantic_named(&widget, Some(show), &Action::Activate)
                });
            if let Err(reason) = opened {
                tracing::debug!(%reason, show, "a page's address names nothing it opens");
            }
        }
        let composition = composition(frame);
        self.page = Page::new();
        Shown {
            title: ui::component_lifecycle::window_title(frame.root_widget())
                .unwrap_or_default()
                .to_owned(),
            css: theme_css(frame.theme()) + PAGE_CSS,
            body: self.page.render(&composition),
        }
    }

    /// What the person did on the element numbered `id` (the semantic `target` it carries),
    /// acted on the view, and what it changed on the page. An element the page numbers for a
    /// part of a widget (a document's block, a control) is acted on through the widget it is a
    /// part of. A press the view refuses changes nothing, and the page is answered what changed
    /// all the same.
    pub fn act(
        &mut self,
        frame: &mut FrameHarness,
        id: u32,
        target: Option<&str>,
        action: &Action,
    ) -> Vec<Patch> {
        if let Some(path) = self.page.path(id) {
            let widget = path
                .split_once('#')
                .map_or(path, |(widget, _)| widget)
                .to_owned();
            if let Err(reason) = frame.perform_semantic_named(&widget, target, action) {
                tracing::debug!(%reason, widget, "a page's press was refused");
            }
        }
        self.changed(frame)
    }

    /// The path of the widget, or the part of one, whose element carries `id`.
    #[must_use]
    pub fn path(&self, id: u32) -> Option<&str> {
        self.page.path(id)
    }

    /// What changed on the page since it last answered: what its instance sent meanwhile (another
    /// person's edit, an action that finished), each element to replace, outermost first.
    pub fn changed(&mut self, frame: &mut FrameHarness) -> Vec<Patch> {
        let composition = composition(frame);
        self.page.patch(&composition)
    }
}

/// `frame` laid out with everything that reached its view, as a window lays out while it is
/// looked at, with a picture of each part a page shows as one.
fn composition(frame: &mut FrameHarness) -> Composition {
    let _still = ui::arriving::still_frames();
    for _ in 0..PAGE_FRAMES {
        if !frame.poll_frame().unwrap_or(false) {
            break;
        }
    }
    let mut composition = frame.composition();
    frame.paint_pictures(&mut composition);
    composition
}

/// The widget whose semantics name `target`, in `composition`.
fn widget_of_target(composition: &Composition, target: &str) -> Option<String> {
    fn find(node: &ui::composition::Composed, target: &str) -> Option<String> {
        if node
            .semantics
            .as_ref()
            .and_then(|semantics| semantics.action_target.as_deref())
            == Some(target)
        {
            return Some(node.path.clone());
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    std::iter::once(&composition.root)
        .chain(&composition.layers)
        .find_map(|node| find(node, target))
}

/// The semantic action `act` is on the view; `None` for what is no node's action (the page's
/// window resized, [`resize`]).
fn semantic(act: PageAction) -> Option<Action> {
    Some(match act {
        PageAction::Activate => Action::Activate,
        PageAction::Toggle => Action::ToggleCheck,
        PageAction::Focus => Action::Focus,
        PageAction::Increment => Action::Increment,
        PageAction::Decrement => Action::Decrement,
        PageAction::Value(value) => Action::SetValue(value),
        PageAction::Select { anchor, focus } => Action::SetSelection {
            anchor: usize::try_from(anchor).unwrap_or(usize::MAX),
            focus: usize::try_from(focus).unwrap_or(usize::MAX),
        },
        PageAction::Resize { .. } => return None,
    })
}

/// Lay `frame` out at its page's size, `width` by `height` CSS pixels (kept between 240 and 8192
/// on a side), as a window lays out at the size it is given: a narrow page shows the view as a
/// narrow window does, and what a node's bounds say is where the page shows it.
pub fn resize(frame: &mut FrameHarness, width: u32, height: u32) {
    let viewport = (
        width.clamp(PAGE_SIDE.0, PAGE_SIDE.1),
        height.clamp(PAGE_SIDE.0, PAGE_SIDE.1),
    );
    if let Err(error) = frame.resize(viewport) {
        tracing::debug!(?error, "a page's view did not lay out at its size");
    }
}

/// Answer the device on the link `link` with `frame`'s view, one ask at a time, until the
/// device lets go of the page.
///
/// # Errors
/// The link fails, or carries something that is no ask.
pub fn serve(
    frame: &mut FrameHarness,
    link: &mut (impl std::io::Read + std::io::Write),
) -> std::io::Result<()> {
    let mut page = ShownOnPage::new();
    while let Some(ask) = window::receive::<WindowAsk>(link)? {
        let answer = match ask {
            WindowAsk::Open { show } => {
                let Shown { title, css, body } = page.open(frame, show.as_deref());
                WindowAnswer::Shown { title, css, body }
            }
            WindowAsk::Act {
                id,
                target,
                action,
                signed,
                invoker,
            } => {
                // The act serves the call its reader signed for it, and only the act does.
                let call = signed
                    .map(|call| serde_json::to_vec(&call))
                    .transpose()
                    .map_err(std::io::Error::other)?;
                let serving = call.is_some();
                if let Some(call) = call {
                    serving_call(frame, call, invoker);
                }
                let size = match action {
                    PageAction::Resize { width, height } => Some((width, height)),
                    _ => None,
                };
                let changed = match semantic(action) {
                    Some(action) => page.act(frame, id, target.as_deref(), &action),
                    None => {
                        // The page's window took a size: the view lays itself out at it.
                        if let Some((width, height)) = size {
                            resize(frame, width, height);
                        }
                        page.changed(frame)
                    }
                };
                if serving {
                    serving_call(frame, Vec::new(), None);
                }
                WindowAnswer::Changed(patches(changed))
            }
            WindowAsk::Changed => WindowAnswer::Changed(patches(page.changed(frame))),
        };
        window::send(link, &answer)?;
    }
    Ok(())
}

fn serving_call(frame: &mut FrameHarness, call: Vec<u8>, invoker: Option<String>) {
    if let Some(window) = frame.widget_mut::<HostedWindow>() {
        window.serve_call(call, invoker);
    }
}

fn patches(patches: Vec<Patch>) -> Vec<PagePatch> {
    patches
        .into_iter()
        .map(|patch| PagePatch {
            id: patch.id,
            html: patch.html,
        })
        .collect()
}

/// The window process a device started to show a view on a web page
/// ([`runtime_portal::process_link::PAGE_ENV`]): the window over its instance's patch connection,
/// answering the device on its link until the device lets go.
///
/// # Errors
/// The variable names no two descriptors, or the window's connections are unusable.
pub fn serve_from_env(named: &str) -> std::io::Result<()> {
    use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
    let unusable =
        |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, what.to_owned());
    let (patch, link) = named
        .split_once(',')
        .and_then(|(patch, link)| Some((patch.parse::<RawFd>().ok()?, link.parse::<RawFd>().ok()?)))
        .ok_or_else(|| unusable("the page's descriptors are not `<patch>,<page>`"))?;
    // SAFETY: the device that started this process handed it these two descriptors, open and
    // its own, for this process alone, and names them in the variable it reads them from.
    let (patch, link) = unsafe { (OwnedFd::from_raw_fd(patch), OwnedFd::from_raw_fd(link)) };
    let channel = Channel::from_socket(patch)?;
    let mut link = std::os::unix::net::UnixStream::from(link);
    let mut frame = frame_over(channel);
    serve(&mut frame, &mut link)
}
