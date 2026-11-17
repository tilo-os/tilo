//! What a page does with files dropped on the shell or uploaded from its menu:
//! copied into the workspace's file tree and opened where they landed. The
//! first frame needs none of it, so the program fetches it the first time a
//! drag reaches the shell ([`DROPS`]).

use ui::kurbo::{Point, Rect};

use crate::browser::Desktop;
use crate::browser::uploads::Picked;

/// The apps whose windows take the bytes of a file dropped on them, where a page hands them over
/// (`ui::portal::dropped`): a note takes a picture, a conversation shares a file, an edit takes a
/// clip. Another app that opens files takes one as its launch's Import, beside its window.
const TAKES_WINDOW_DROPS: [&str; 3] = ["so.tilo.notes", "so.tilo.chat", "so.tilo.video-editor"];

/// Whether a window of `app` takes the bytes of files dropped on it itself, as its window's drop
/// does: the page hands them to it besides copying them into the workspace's files.
pub(crate) fn hands_files(app: &str) -> bool {
    TAKES_WINDOW_DROPS.contains(&app)
}

/// Whether a window of `app` takes files dropped on it: by its own window's drop, or opened beside
/// it in the app as its Import takes a file.
pub(crate) fn takes_files(app: &str) -> bool {
    app == fs_protocol::tree::FILES_APP
        || TAKES_WINDOW_DROPS.contains(&app)
        || tilo_content_types::landing_in(app).is_some()
}

/// Where files dropped at a point of the shell go.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DropTarget {
    /// The window of `tab`, standing at `rect`, whose app is `app`.
    Window {
        tab: u64,
        rect: Rect,
        app: Option<String>,
    },
    /// Between the strip's windows, or anywhere on a shell with none: a window opens at `slot`,
    /// and `area` is what the page outlines (a bar in the gap, or the whole empty shell).
    Strip {
        slot: tilo_desktop_surface::StripPart,
        area: Rect,
    },
    /// Nothing there takes files, and why.
    Refused(String),
}

/// What a page asks of its file drops, as the trait object [`DROPS`] makes.
pub(crate) trait DropsApi {
    /// Where files dropped at `point` of `desktop` (the surface's coordinates) go, the shell being
    /// `size`.
    fn drop_target(&self, desktop: &Desktop, point: Point, size: (f64, f64)) -> DropTarget;
    /// Copy `files` into the workspace's file tree and open them where they fell.
    fn drop_files(
        &self,
        desktop: &Desktop,
        files: Vec<Picked>,
        target: &DropTarget,
        progress: js_sys::Function,
    ) -> Result<(), String>;
    /// Copy `files` chosen with the shell menu into the workspace's file tree.
    fn upload(
        &self,
        desktop: &Desktop,
        files: Vec<Picked>,
        progress: js_sys::Function,
    ) -> Result<(), String>;
}

/// The drops' code, as a program that fetches it on first use hands it over.
#[derive(Clone, Copy)]
pub struct MakeDrops(fn() -> Box<dyn DropsApi>);

impl MakeDrops {
    pub(crate) fn make(self) -> Box<dyn DropsApi> {
        (self.0)()
    }
}

/// The drops this build links.
#[must_use]
pub fn builtin_drops() -> MakeDrops {
    MakeDrops(|| Box::new(crate::browser::drops::Drops))
}

ui::lazy_code! {
    /// The drops' code: linked, or with `deferred-drops` fetched the first time a drag reaches
    /// the shell.
    pub static DROPS: MakeDrops = builtin_drops, feature = "deferred-drops";
}
