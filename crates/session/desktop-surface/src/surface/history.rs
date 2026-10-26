//! A workspace's history, served to the Activity app: the activity model
//! (What's new, History and Replay) reading a host's retained history, as
//! a native session serves its own ([`crate::session::ActivityRequests`]).
//!
//! The model only reads the history: it works on an [`ActivitySnapshot`]
//! the host built from its admitted room document, and on detached copies
//! of the records the workspace's windows show. What it keeps of its own is
//! what this device has seen, through the host ([`HistorySource::host`]).
//!
//! The surface reaches the model only through [`open_activity`], which a
//! host names in [`HistorySource::activity_pane`]. A host that would rather
//! not carry the model's code from the start (the browser desktop, which
//! fetches it as its own module the first time someone opens Activity)
//! answers [`ActivityPaneCode::Loading`] until it has it.
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_desktop_model::{DesktopTree, TabId, WorkspaceId};

use crate::activity::{
    ActivityHost, ActivitySnapshot, ActivityView as PaneView, DetachedRecordHistory,
    RoomActivityPane,
};
use crate::navigation::ActivityView;
use crate::session::ActivityReads;

/// Where a surface's workspace history comes from. Without one, Catch up,
/// Explore and Replay stay unavailable.
pub trait HistorySource {
    /// `workspace`'s admitted history: the desktop together with the room
    /// document whose changes are its history. `None` when the host retains
    /// none for it.
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Option<ActivitySnapshot>;

    /// Detached copies of the records `workspace`'s windows show, each with
    /// its whole change history.
    fn record_histories(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Vec<DetachedRecordHistory>;

    /// Some retained desktop history exceeded the host's projection budget.
    fn history_is_partial(&self) -> bool {
        false
    }

    /// What the model may ask of the host: identity, the device's private
    /// seen frontiers, application names.
    fn host(&self) -> Rc<dyn ActivityHost>;

    /// Name the authors the host knows before the model reads changes.
    fn register_authors(&self, _authors: &mut ui::AuthorDirectory) {}

    /// The window showing `target`, focused when the person opens it from
    /// the history.
    fn resource_tab(
        &self,
        _tree: &DesktopTree,
        _target: &ui::HistoryResourceTarget,
    ) -> Option<TabId> {
        None
    }

    /// Changes whenever a record the history reads changed, so an open
    /// model reads again.
    fn revision(&self) -> u64 {
        0
    }

    /// The code that opens the model: linked in, unless the host overrides
    /// this. Asked each time the model is to open, so a host still loading
    /// it answers [`ActivityPaneCode::Loading`] and wakes the surface once
    /// it can answer [`ActivityPaneCode::Ready`].
    fn activity_pane(&self) -> ActivityPaneCode {
        ActivityPaneCode::Ready(open_activity)
    }
}

/// Where the activity model's code is, as a [`HistorySource`] reports it.
#[derive(Clone, Copy, Debug)]
pub enum ActivityPaneCode {
    /// Open the model with this, normally [`open_activity`].
    Ready(OpenActivity),
    /// The host is still loading it: what the windows asked waits.
    Loading,
    /// The host could not load it.
    Unavailable,
}

/// [`open_activity`]'s signature, for a host that hands it over at run time.
pub type OpenActivity =
    fn(&mut dyn HistorySource, &DesktopTree, u64, WorkspaceId) -> Option<ActivityModel>;

/// An open activity model, as [`open_activity`] hands it to the surface.
pub struct ActivityModel(pub(super) Box<dyn Served>);

/// Open the activity model of `workspace`, on the history `source` keeps
/// for the desktop `tree` at `revision`. `None` when `source` retains no
/// history for that workspace.
pub fn open_activity(
    source: &mut dyn HistorySource,
    tree: &DesktopTree,
    revision: u64,
    workspace: WorkspaceId,
) -> Option<ActivityModel> {
    WorkspaceActivity::open(source, tree, revision, workspace)
        .map(|model| ActivityModel(Box::new(model)))
}

/// What the surface asks of an open model. The surface calls it only
/// through the model's vtable, so none of the model's code is reachable
/// from the surface itself.
pub(super) trait Served: ActivityReads {
    /// The workspace whose history the model holds.
    fn workspace(&self) -> WorkspaceId;
    /// Read the history again when the desktop or a record changed since.
    fn follow(&mut self, source: &mut dyn HistorySource, tree: &DesktopTree, revision: u64);
    /// Run write `action` with `arguments`; the record it opens, if any.
    fn act(
        &mut self,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Option<ui::HistoryResourceTarget>, String>;
    /// The person began reading what is new.
    fn begin_summary(&mut self);
    /// They stopped: what they were shown counts as seen.
    fn end_summary(&mut self);
    /// Offer `view` to the app, at only what `author` did when named.
    fn focus(&mut self, view: ActivityView, author: Option<String>);
    /// Whether a view is offered that the app has not shown yet.
    fn offers_focus(&self) -> bool;
    /// The model closes: remember what this device has now seen.
    fn finish(&mut self);
}

fn pane_view(view: ActivityView) -> PaneView {
    match view {
        ActivityView::CatchUp => PaneView::CatchUp,
        ActivityView::Explore => PaneView::Explore,
        ActivityView::Replay => PaneView::Replay,
    }
}

/// The activity model and the history it reads.
struct WorkspaceActivity {
    pane: RoomActivityPane,
    snapshot: ActivitySnapshot,
    workspace: WorkspaceId,
    /// The tree and source revisions the model last read.
    read: (u64, u64),
}

impl WorkspaceActivity {
    fn open(
        source: &mut dyn HistorySource,
        tree: &DesktopTree,
        revision: u64,
        workspace: WorkspaceId,
    ) -> Option<Self> {
        let snapshot = source.workspace_history(tree, workspace)?;
        let mut pane = RoomActivityPane::new();
        pane.set_host(source.host());
        pane.load_frontiers();
        let mut model = Self {
            pane,
            snapshot,
            workspace,
            read: (revision, source.revision()),
        };
        model.read_records(source, tree);
        Some(model)
    }

    fn read_records(&mut self, source: &mut dyn HistorySource, tree: &DesktopTree) {
        let records = source.record_histories(tree, self.workspace);
        source.register_authors(self.pane.authors_mut());
        self.pane.refresh(&mut self.snapshot);
        self.pane
            .refresh_detached_records(&mut self.snapshot, records);
        if source.history_is_partial() {
            self.pane.mark_desktop_history_capped();
        }
    }
}

impl ActivityReads for WorkspaceActivity {
    fn revision(&self) -> u64 {
        self.pane.served_revision()
    }

    fn read(&mut self, action: &str, arguments: &Map<String, Value>) -> Result<Value, String> {
        self.pane
            .serve_read(action, arguments, &mut &mut self.snapshot, None)
    }
}

impl Served for WorkspaceActivity {
    fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    fn follow(&mut self, source: &mut dyn HistorySource, tree: &DesktopTree, revision: u64) {
        let now = (revision, source.revision());
        if now == self.read {
            return;
        }
        self.read = now;
        if let Some(snapshot) = source.workspace_history(tree, self.workspace) {
            self.snapshot = snapshot;
        }
        self.read_records(source, tree);
    }

    fn act(
        &mut self,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Option<ui::HistoryResourceTarget>, String> {
        self.pane
            .serve_action(action, arguments, &mut self.snapshot, None)
            .map(|outcome| outcome.open_target)
    }

    fn begin_summary(&mut self) {
        self.pane.begin_summary(self.workspace.raw());
    }

    fn end_summary(&mut self) {
        self.pane.end_summary(self.workspace.raw());
    }

    fn focus(&mut self, view: ActivityView, author: Option<String>) {
        match author {
            Some(author) => self
                .pane
                .focus_deep_link(&crate::activity::ActivityDeepLink {
                    author: Some(author),
                    ..crate::activity::ActivityDeepLink::default()
                }),
            None => self.pane.focus_view(pane_view(view)),
        }
    }

    fn offers_focus(&self) -> bool {
        self.pane.serve_focus().is_some()
    }

    fn finish(&mut self) {
        self.pane.finish_view(self.workspace.raw());
    }
}
