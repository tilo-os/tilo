//! Windows an agent asks for on a workspace's layout. The open is answered at
//! once, so an action never waits for a window; the window is placed as soon
//! as this service holds the layout, which may arrive after the open did.

use std::time::Instant;

use tilo_desktop_model::ownership::OpenedBy;

use super::{DocumentMutation, JOIN_WAIT, Worker, WorkspaceDocuments, WorkspaceId};

/// A window asked for on a workspace's layout and not placed yet.
pub(super) struct Show {
    /// The workspace, as its layout's route names it.
    workspace: String,
    app: String,
    title: String,
    document: String,
    person: String,
    opened_by: Option<OpenedBy>,
    /// Past this, the window is no longer placed: the open said only that it
    /// was taken.
    until: Instant,
}

impl Show {
    pub(super) fn new(
        workspace: &str,
        app: &str,
        title: String,
        document: String,
        person: String,
        opened_by: Option<OpenedBy>,
    ) -> Self {
        Self {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            title,
            document,
            person,
            opened_by,
            until: Instant::now()
                .checked_add(JOIN_WAIT)
                .unwrap_or_else(Instant::now),
        }
    }
}

impl Worker {
    /// The layout of `workspace_id` as this service holds it, if it does.
    fn layout_held(&self, workspace_id: WorkspaceId) -> Option<automerge::AutoCommit> {
        self.workspaces
            .get(&workspace_id)?
            .records
            .as_ref()
            .and_then(WorkspaceDocuments::desktop)
    }

    /// Place the windows asked for on `workspace_id`'s layout, once this
    /// service holds it; until then they wait for the workspace's next
    /// change, and past their wait they are dropped. The error is why a
    /// placement could not be written.
    pub(super) fn place_shows(&mut self, workspace_id: WorkspaceId) -> Result<(), String> {
        let Some(mut shows) = self.shows.remove(&workspace_id) else {
            return Ok(());
        };
        let now = Instant::now();
        shows.retain(|show| show.until > now);
        let Some(first) = shows.first() else {
            return Ok(());
        };
        let layout = crate::member::desktop_route(&first.workspace);
        if self.layout_held(workspace_id).is_none() {
            // The layout was asked for with the open; its arrival is the change that wakes
            // this thread.
            self.shows.insert(workspace_id, shows);
            self.awaiting(workspace_id, true);
            return Ok(());
        }
        let mut failed = Ok(());
        for show in shows {
            // Read again for each: the window placed before changed it.
            let Some(held) = self.layout_held(workspace_id) else {
                break;
            };
            let view = tilo_desktop_model::place::View {
                app_id: &show.app,
                title: &show.title,
                record: &show.document,
                beside: None,
            };
            let opener = tilo_desktop_model::place::Opener {
                person: &show.person,
                run: show.opened_by,
            };
            let Some(change) = tilo_desktop_model::place::place_view(&held, &opener, &view) else {
                continue;
            };
            if let Err(reason) = self.commit(
                workspace_id,
                &layout,
                &DocumentMutation::Incremental(change),
            ) {
                tracing::warn!(%reason, app = show.app, "a window asked for on the layout was not placed");
                failed = failed.and(Err(reason));
            }
        }
        failed
    }
}
