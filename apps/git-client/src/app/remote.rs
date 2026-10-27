//! The repository's operations as the app runs them: re-reading the model after a mutation, running an owned action, and the results of the remote jobs a start begins.

use super::*;

impl GitClientApp {
    /// Re-read the live repository into the model after a mutation, so the
    /// UI reflects the *actual* on-disk state (proving the op took effect).
    /// A no-op for the sample-backed app. Preserves the selected screen and
    /// clamps stale selections.
    pub(super) fn reload(&mut self) {
        let Some(path) = self.repo_path.clone() else {
            return;
        };
        if let Some((model, diffs)) = crate::repo::load(&path) {
            self.model = model;
            self.diffs = diffs;
            self.graph_layout = graph::layout(&mut self.model.commits);
            self.model.graph_width = self.graph_layout.lanes;
            self.selected_change = first_change(&self.model);
            if self.selected_commit >= self.model.commits.len() {
                self.selected_commit = 0;
            }
            // A branch the person deleted is no longer under the keyboard:
            // keep the selection on a branch, not past the end of the list.
            let local = self.model.branches.iter().filter(|b| !b.is_remote).count();
            self.branch_selection = self
                .branch_selection
                .map(|index| index.min(local.saturating_sub(1)))
                .filter(|_| local > 0);
            self.diff_scroll = 0.0;
        }
    }

    /// Run an action and record any error for the status line. On success
    /// the model is reloaded from disk so the render reflects reality.
    pub(super) fn run_action(&mut self, result: anyhow::Result<()>) {
        match result {
            Ok(()) => {
                self.last_error = None;
                self.notice = None;
                self.reload();
            }
            Err(e) => self.report_error(&e),
        }
    }

    /// Report a failed action, in place of any notice.
    pub(super) fn report_error(&mut self, error: &anyhow::Error) {
        self.notice = None;
        self.last_error = Some(error.to_string());
    }

    /// The repo path for actions; `None` means sample-backed (no real repo).
    pub(super) fn repo(&self) -> Option<std::path::PathBuf> {
        self.repo_path.clone()
    }

    pub(super) fn repository_busy(&mut self) -> bool {
        if self.remote_job.is_some() {
            self.last_error = Some(ui::i18n!("git-remote-busy"));
            return true;
        }
        false
    }

    pub(super) fn start_remote(
        &mut self,
        action: RemoteAction,
        remote: Option<String>,
        reply: Option<EffectReply>,
    ) -> Result<(), String> {
        if self.remote_job.is_some() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let path = self
            .repo()
            .ok_or_else(|| "remote operation requires a repository".to_string())?;
        let label = match action {
            RemoteAction::Fetch => "Fetching…",
            RemoteAction::Pull => "Pulling…",
            RemoteAction::Push => "Pushing…",
        }
        .to_string();
        // The worker's reply wakes the app; nothing polls frame by frame.
        let (sender, receiver) = ui::wake::waking_channel();
        std::thread::Builder::new()
            .name("git-remote".into())
            .spawn(move || {
                let result = match action {
                    RemoteAction::Fetch => crate::repo::fetch(&path, remote.as_deref()).map(|_| ()),
                    RemoteAction::Pull => crate::repo::pull(&path, remote.as_deref()),
                    RemoteAction::Push => crate::repo::push(&path, remote.as_deref()),
                }
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        self.last_error = None;
        self.remote_job = Some(RemoteJob {
            receiver,
            reply,
            label,
        });
        Ok(())
    }

    pub(super) fn drain_remote(&mut self) {
        let Some(job) = self.remote_job.as_mut() else {
            return;
        };
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(ui::i18n!("git-remote-worker-stopped")),
        };
        self.last_error = result.as_ref().err().cloned();
        // libgit2 operations are not cancellable: the worker's actual outcome
        // is the answer, so a push the server accepted is never reported as
        // cancelled.
        if let Some(reply) = self.remote_job.take().and_then(|job| job.reply) {
            reply.finish(result.map(|()| ActionOutput::default()));
        }
        // A failed operation can still have changed objects/refs. Refresh from
        // disk on either outcome; never infer rollback from an error.
        self.reload();
    }
}
