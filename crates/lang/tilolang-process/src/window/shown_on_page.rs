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

use tilo_server_protocol::page::window::{self, WindowAnswer, WindowAsk};
use tilo_server_protocol::page::{PageAction, PagePatch, PointerEvent, PointerKind};
use ui::a11y::Action;
use ui::composition::Composition;
use ui::composition::page::{PAGE_CSS, Page, Patch, theme_css};
use ui::headless::Driver;

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
    /// The view declares `protect capture`: a capture or a stream of the page leaves it out.
    pub capture_protected: bool,
}

/// The frame a page lays out `window` in, where nobody draws it.
#[must_use]
pub fn frame_of(window: HostedWindow) -> Driver {
    Driver::new(ui::State::new(), Box::new(window), VIEWPORT)
}

/// The frame a page lays out the window over the patch connection `channel` in, whose other end
/// its instance's host holds.
#[must_use]
pub fn frame_over(channel: super::hosted::PatchConnection) -> Driver {
    frame_of(HostedWindow::over(channel))
}

/// A window's frame shown on one page: the numbers its elements carry from one answer to the
/// next. See the module docs.
#[derive(Default)]
pub struct ShownOnPage {
    page: Page,
    /// What the device stands in each of the page's wells, by place ([`Page::wells`]).
    wells: std::collections::HashMap<String, String>,
}

impl ShownOnPage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The page whole of `frame`, after activating `show` where the page's address names
    /// something the view shows (an item of its list). An address that names nothing it shows
    /// opens the view as it stands.
    pub fn open(&mut self, frame: &mut Driver, show: Option<&str>) -> Shown {
        // The page opens on the first view its instance sends, not on the window waiting for it:
        // a page answered before would show an empty window and no title.
        await_app(frame, ui::widget::AwaitsContent::awaits_content);
        if let Some(show) = show {
            let composition = composition(frame);
            let opened = widget_of_target(&composition, show)
                .ok_or_else(|| "nothing the view shows".to_owned())
                .and_then(|(widget, named)| {
                    // The widget answers for the target it carries: a row's own target opens the
                    // item it presents, whichever form of the resource's id the page named.
                    frame.perform_semantic_named(&widget, named.as_deref(), &Action::Activate)
                });
            if let Err(reason) = opened {
                tracing::debug!(%reason, show, "a page's address names nothing it opens");
            }
        }
        let composition = composition(frame);
        self.page = Page::new();
        self.page.wells.clone_from(&self.wells);
        Shown {
            title: ui::component_lifecycle::window_title(frame.root_widget())
                .unwrap_or_default()
                .to_owned(),
            css: theme_css(frame.theme()) + PAGE_CSS,
            body: self.page.render(&composition),
            capture_protected: frame.capture_protected(),
        }
    }

    /// What the person did on the element numbered `id` (the semantic `target` it carries),
    /// acted on the view ([`Page::act`]), or the size the page's window now is, and what it
    /// changed on the page. A press the view refuses changes nothing, and the page is answered
    /// what changed all the same.
    pub fn act(
        &mut self,
        frame: &mut Driver,
        id: u32,
        target: Option<&str>,
        action: PageAction,
    ) -> Vec<Patch> {
        let acted = match action {
            PageAction::Resize { width, height } => {
                resize(frame, width, height);
                Ok(())
            }
            action => self
                .page
                .semantic_target(id, action)
                .and_then(|(widget, action)| frame.perform_semantic_named(widget, target, &action)),
        };
        if let Err(reason) = acted {
            tracing::debug!(%reason, id, "a page's press was refused");
        }
        // The page is answered once its instance ran the press, with what that changed.
        await_app(frame, |window| {
            window
                .view()
                .is_some_and(tilo_app_window::AppWindow::presses_unanswered)
        });
        self.changed(frame)
    }

    /// The pointer at `event`, carried into the view's widgets as a device's pointer is: a drag
    /// on the page runs the same gesture the view runs natively, and the arranging action it
    /// commits is the one a device commits. What it changed.
    pub fn pointer(&mut self, frame: &mut Driver, event: PointerEvent) -> Vec<Patch> {
        use ui::winit::dpi::PhysicalPosition;
        use ui::winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        let device_id = DeviceId::dummy();
        #[allow(
            clippy::cast_precision_loss,
            reason = "a pointer's logical pixel is well inside f64's exact range"
        )]
        let position = PhysicalPosition::new(event.x as f64, event.y as f64);
        match event.kind {
            PointerKind::Down => {
                frame.dispatch_to_root(&WindowEvent::CursorMoved {
                    device_id,
                    position,
                });
                frame.dispatch_to_root(&WindowEvent::MouseInput {
                    device_id,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                });
            }
            PointerKind::Move => {
                frame.dispatch_to_root(&WindowEvent::CursorMoved {
                    device_id,
                    position,
                });
            }
            PointerKind::Up => {
                frame.dispatch_to_root(&WindowEvent::CursorMoved {
                    device_id,
                    position,
                });
                frame.dispatch_to_root(&WindowEvent::MouseInput {
                    device_id,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                });
            }
        }
        self.changed(frame)
    }

    /// Draw the page in the light or dark look `dark` names, as a window follows the system it
    /// runs on: the whole page again, in the theme's tokens.
    pub fn appearance(&mut self, frame: &mut Driver, dark: bool) -> Shown {
        frame.set_theme(if dark {
            ui::Theme::dark()
        } else {
            ui::Theme::light()
        });
        self.open(frame, None)
    }

    /// What changed on the page once it shows `wells` in its wells, by place: what the device
    /// stands where the view leaves a window to it.
    pub fn fill_wells(
        &mut self,
        frame: &mut Driver,
        wells: std::collections::BTreeMap<String, String>,
    ) -> Vec<Patch> {
        self.wells = wells.into_iter().collect();
        self.page.wells.clone_from(&self.wells);
        self.changed(frame)
    }

    /// The path of the widget, or the part of one, whose element carries `id`.
    #[must_use]
    pub fn path(&self, id: u32) -> Option<&str> {
        self.page.path(id)
    }

    /// What changed on the page since it last answered: what its instance sent meanwhile (another
    /// person's edit, an action that finished), each element to replace, outermost first.
    pub fn changed(&mut self, frame: &mut Driver) -> Vec<Patch> {
        let composition = composition(frame);
        self.page.patch(&composition)
    }
}

/// Lay `frame` out as what its instance sends arrives, while `waits` holds of its window.
fn await_app(frame: &mut Driver, waits: impl Fn(&HostedWindow) -> bool) {
    while let Some(window) = frame.widget_mut::<HostedWindow>()
        && waits(window)
        && window.wait_for_app()
    {
        frame.woken();
    }
}

/// `frame` laid out with everything that reached its view, as a window lays out while it is
/// looked at, with a picture of each part a page shows as one.
fn composition(frame: &mut Driver) -> Composition {
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

/// The widget that shows the resource `target`, in `composition`, and the target its own
/// activation answers: a resource is named by its published address (`tl:<app>:<id>`, what a
/// catalogue row and a link give) as its app names it (`<id>`), and both name the one resource.
/// A row a list draws names the resource it presents as the node's item, and its own activation
/// target is the list's (`key:…`), which opens that item.
fn widget_of_target(composition: &Composition, target: &str) -> Option<(String, Option<String>)> {
    fn find(node: &ui::composition::Composed, target: &str) -> Option<(String, Option<String>)> {
        let semantics = node.semantics.as_ref();
        if let Some(named) = semantics.and_then(|semantics| semantics.action_target.as_deref())
            && names_one_resource(named, target)
        {
            return Some((node.path.clone(), Some(named.to_owned())));
        }
        if let Some(item) = semantics.and_then(|semantics| semantics.item.as_deref())
            && names_one_resource(item, target)
        {
            let named = semantics.and_then(|semantics| semantics.action_target.clone());
            return Some((node.path.clone(), named));
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    std::iter::once(&composition.root)
        .chain(&composition.layers)
        .find_map(|node| find(node, target))
}

/// Whether the targets `one` and `other` name the same resource: the same name, a published
/// address and the id inside it (`tl:<app>:<id>` and `<id>`), or a row's target, which carries
/// the id of the item it presents after the list's own key (`key:<widget>~<widget>/<id>`).
fn names_one_resource(one: &str, other: &str) -> bool {
    if one == other {
        return true;
    }
    let (carried, named) = (inside_address(one), inside_address(other));
    if carried == named {
        return true;
    }
    // A list row's target is the list's key, then `~`, then the widget's own key, then the id
    // of the item the row presents.
    let row = carried.split_once('~').map_or(carried, |(_, after)| after);
    let item = row.split_once('/').map_or(row, |(_, item)| item);
    item == named
}

/// `name` without the app a published address carries (`tl:<app>:<id>` is `<id>`); a name that is
/// no published address is itself.
fn inside_address(name: &str) -> &str {
    name.strip_prefix("tl:")
        .and_then(|rest| rest.split_once(':'))
        .filter(|(app, id)| !app.is_empty() && !id.is_empty())
        .map_or(name, |(_, id)| id)
}

/// Lay `frame` out at its page's size, `width` by `height` CSS pixels (kept between 240 and 8192
/// on a side), as a window lays out at the size it is given: a narrow page shows the view as a
/// narrow window does, and what a node's bounds say is where the page shows it.
pub fn resize(frame: &mut Driver, width: u32, height: u32) {
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
    frame: &mut Driver,
    link: &mut (impl std::io::Read + std::io::Write),
) -> std::io::Result<()> {
    let mut page = ShownOnPage::new();
    while let Some(ask) = window::receive::<WindowAsk>(link)? {
        let answer = match ask {
            WindowAsk::Open { show } => {
                let Shown {
                    title, css, body, ..
                } = page.open(frame, show.as_deref());
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
                let changed = page.act(frame, id, target.as_deref(), action);
                if serving {
                    serving_call(frame, Vec::new(), None);
                }
                WindowAnswer::Changed(patches(changed))
            }
            WindowAsk::Changed => WindowAnswer::Changed(patches(page.changed(frame))),
            WindowAsk::Wells(wells) => {
                WindowAnswer::Changed(patches(page.fill_wells(frame, wells)))
            }
            WindowAsk::Pointer(event) => WindowAnswer::Changed(patches(page.pointer(frame, event))),
            WindowAsk::Appearance { dark } => {
                let Shown {
                    title, css, body, ..
                } = page.appearance(frame, dark);
                WindowAnswer::Shown { title, css, body }
            }
        };
        window::send(link, &answer)?;
    }
    Ok(())
}

fn serving_call(frame: &mut Driver, call: Vec<u8>, invoker: Option<String>) {
    if let Some(window) = frame.widget_mut::<HostedWindow>() {
        window.serve_call(call, invoker);
    }
}

fn patches(patches: Vec<Patch>) -> Vec<PagePatch> {
    patches
        .into_iter()
        .map(|patch| match patch {
            Patch::Replace { id, html } => PagePatch::Replace { id, html },
            Patch::Insert { parent, at, html } => PagePatch::Insert { parent, at, html },
            Patch::Remove { id } => PagePatch::Remove { id },
        })
        .collect()
}

/// The window process a device started to show a view on a web page
/// ([`runtime_portal::process_link::PAGE_ENV`]): the window over its instance's patch connection,
/// answering the device on its link until the device lets go.
///
/// # Errors
/// The variable names no two descriptors, or the window's connections are unusable.
#[cfg(unix)]
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
    let channel = tilo_render_protocol::channel::Channel::from_socket(patch)?;
    let mut link = std::os::unix::net::UnixStream::from(link);
    let mut frame = frame_over(channel);
    serve(&mut frame, &mut link)
}

#[cfg(test)]
mod tests {
    use super::{inside_address, names_one_resource};

    /// A page address naming a resource by its published address opens the row the app names by
    /// its own id inside it, and either form names the one resource.
    #[test]
    fn a_published_address_and_the_id_inside_it_name_one_resource() {
        assert_eq!(inside_address("tl:so.tilo.migrate:root-a/i1"), "root-a/i1");
        assert_eq!(inside_address("root-a/i1"), "root-a/i1");
        assert!(names_one_resource(
            "tl:so.tilo.migrate:root-a/i1",
            "root-a/i1"
        ));
        assert!(names_one_resource(
            "root-a/i1",
            "tl:so.tilo.migrate:root-a/i1"
        ));
        assert!(!names_one_resource(
            "tl:so.tilo.migrate:root-a/i1",
            "root-a/i2"
        ));
        assert_eq!(inside_address("devices/abc/migrate"), "devices/abc/migrate");
    }

    /// A list row names the item it presents after the list's own key and the widget's, so a
    /// page address naming that item by its resource id opens the row.
    #[test]
    fn a_lists_row_target_names_the_item_it_presents() {
        assert!(names_one_resource(
            "key:list.t142.body~list.for3/root-a/i1",
            "tl:so.tilo.migrate:root-a/i1"
        ));
        assert!(names_one_resource(
            "key:list.t142.body~list.for3/root-a/i1",
            "root-a/i1"
        ));
        assert!(!names_one_resource(
            "key:list.t142.body~list.for3/root-a/i1",
            "tl:so.tilo.migrate:root-a/i2"
        ));
        assert!(!names_one_resource("root-a/i1/check", "root-a/i1"));
    }
}
