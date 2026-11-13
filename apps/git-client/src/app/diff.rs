//! What the selection becomes: the diff a row names, staging and unstaging a file
//! or a hunk, the commit, discard, and the branch verbs.

use super::{
    ChangeArea, DiffKey, Field, FileChange, GitClientState, Invoke, RemoteAction, Screen, Widget,
    first_change, graph,
};

impl GitClientState {
    /// The diff-map key for the currently selected change, if any. Resolves
    /// the staged vs. worktree side from the selected area so a partially
    /// staged file shows the correct diff on each side.
    pub(super) fn current_diff_key(&self) -> Option<DiffKey> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        Some(DiffKey::for_area(area, list.get(idx)?.path.clone()))
    }

    pub(super) fn selected_is_staged(&self) -> bool {
        matches!(self.selected_change, Some((ChangeArea::Staged, _)))
    }

    /// Stage or unstage a single file: against a real repository through `git2`, then re-read the
    /// model, and for the sample as an in-memory move so the capture still shows the flow.
    pub(super) fn toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
        if let Some(path) = self.repo() {
            let list = match area {
                ChangeArea::Staged => &self.model.staged,
                ChangeArea::Unstaged => &self.model.unstaged,
                ChangeArea::Untracked => &self.model.untracked,
            };
            let Some(file) = list.get(idx).map(|f| f.path.clone()) else {
                return;
            };
            let res = match area {
                ChangeArea::Staged => crate::repo::unstage_file(&path, &file),
                ChangeArea::Unstaged | ChangeArea::Untracked => {
                    crate::repo::stage_file(&path, &file)
                }
            };
            self.run_action(res);
            return;
        }
        self.toggle_stage_in_memory(area, idx);
    }

    pub(super) fn toggle_stage_in_memory(&mut self, area: ChangeArea, idx: usize) {
        match area {
            ChangeArea::Staged => {
                if idx < self.model.staged.len() {
                    let mut f = self.model.staged.remove(idx);
                    f.area = ChangeArea::Unstaged;
                    self.model.unstaged.push(f);
                }
            }
            ChangeArea::Unstaged => {
                if idx < self.model.unstaged.len() {
                    let mut f = self.model.unstaged.remove(idx);
                    f.area = ChangeArea::Staged;
                    self.model.staged.push(f);
                }
            }
            ChangeArea::Untracked => {
                if idx < self.model.untracked.len() {
                    let mut f = self.model.untracked.remove(idx);
                    f.area = ChangeArea::Staged;
                    f.kind = crate::model::ChangeKind::Added;
                    self.model.staged.push(f);
                }
            }
        }
        self.selected_change = first_change(&self.model);
    }

    pub(super) fn stage_all(&mut self) {
        if self.repository_busy() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::stage_all(&path);
            self.run_action(res);
            return;
        }
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.unstaged.drain(..) {
            f.area = ChangeArea::Staged;
            moved.push(f);
        }
        for mut f in self.model.untracked.drain(..) {
            f.area = ChangeArea::Staged;
            f.kind = crate::model::ChangeKind::Added;
            moved.push(f);
        }
        self.model.staged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    pub(super) fn unstage_all(&mut self) {
        if self.repository_busy() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::unstage_all(&path);
            self.run_action(res);
            return;
        }
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.staged.drain(..) {
            f.area = ChangeArea::Unstaged;
            moved.push(f);
        }
        self.model.unstaged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    /// Whether there is something staged and a message to commit it with.
    pub(super) fn can_commit(&self) -> bool {
        !self.model.staged.is_empty() && !self.commit_input.value().trim().is_empty()
    }

    pub(super) fn do_commit(&mut self) {
        if self.repository_busy() {
            return;
        }
        if self.model.staged.is_empty() || self.commit_input.value().trim().is_empty() {
            return;
        }
        if let Some(path) = self.repo() {
            let msg = self.commit_input.value().to_string();
            let res = crate::repo::commit(&path, &msg).map(|_| ());
            let ok = res.is_ok();
            self.run_action(res);
            if ok {
                self.commit_input.set_text("");
                self.set_focus(None);
                // Land on the History screen so the new commit is visible.
                self.switch_screen(Screen::History);
                self.selected_commit = 0;
                self.history_scroll = 0.0;
            }
            return;
        }
        self.do_commit_in_memory();
    }

    /// Sample-only commit: synthesise a tip commit so the capture shows the effect without a
    /// repository.
    pub(super) fn do_commit_in_memory(&mut self) {
        let staged = std::mem::take(&mut self.model.staged);
        let parent = self.model.commits.first().map(|c| c.id.clone());
        let id = format!("{:07x}", self.model.commits.len() * 0x1f7 + 0xa10);
        let message = self.commit_input.value().to_string();
        let summary = message.lines().next().unwrap_or("").to_string();
        let body = message
            .split_once('\n')
            .map(|x| x.1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let new = crate::model::Commit {
            id,
            summary,
            body,
            author_name: self.model.author_name.clone(),
            author_email: self.model.author_email.clone(),
            when: "just now".into(),
            time: chrono::Utc::now().timestamp(),
            parents: parent.into_iter().collect(),
            refs: vec![
                crate::model::RefLabel {
                    name: "HEAD".into(),
                    kind: crate::model::RefKind::Head,
                },
                crate::model::RefLabel {
                    name: self.model.head.clone(),
                    kind: crate::model::RefKind::LocalBranch,
                },
            ],
            lane: 0,
            files: staged,
        };
        if let Some(prev) = self.model.commits.first_mut() {
            prev.refs
                .retain(|r| r.kind != crate::model::RefKind::Head && r.name != self.model.head);
        }
        self.model.commits.insert(0, new);
        self.graph_layout = graph::layout(&mut self.model.commits);
        self.model.graph_width = self.graph_layout.lanes;
        self.commit_input.set_text("");
        self.set_focus(None);
        self.selected_change = first_change(&self.model);
    }

    /// Discard a file's worktree changes (revert unstaged edits, or delete an untracked file); the
    /// controls reach here only through the confirmation sheet.
    pub(super) fn discard_file(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
        let Some(path) = self.repo() else { return };
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        let Some(file) = list.get(idx).map(|f| f.path.clone()) else {
            return;
        };
        let res = crate::repo::discard_file(&path, &file);
        self.run_action(res);
    }

    /// Discard a single hunk of the selected (unstaged) file's worktree change.
    pub(super) fn discard_hunk(&mut self, hunk: usize) {
        if self.repository_busy() {
            return;
        }
        let Some((area, _)) = self.selected_change else {
            return;
        };
        // Only worktree hunks are discardable: a staged file's diff is the index side, which
        // discard does not act on.
        if area == ChangeArea::Staged {
            return;
        }
        if let (Some(path), Some(file)) = (self.repo(), self.current_path()) {
            let res = crate::repo::discard_hunk(&path, &file, hunk);
            self.run_action(res);
        }
    }

    /// Open the repository at the empty-state path field, binding it if valid.
    pub(super) fn open_repo_from_input(&mut self) {
        let raw = self.open_input.value().to_owned();
        if let Ok(path) = self.typed_repository(&raw) {
            let _ = self.open_repository_at(&path);
        }
    }

    /// The repository path the person typed, or why it names none (said in the window too).
    pub(super) fn typed_repository(
        &mut self,
        raw: &str,
    ) -> Result<ui::tilo_resource::AbsolutePath, String> {
        let raw = raw.trim();
        let typed = if raw.is_empty() {
            Err("no repository path was given".to_owned())
        } else {
            ui::tilo_resource::AbsolutePath::new(raw).map_err(String::from)
        };
        if let Err(error) = &typed {
            self.last_error = Some(error.clone());
        }
        typed
    }

    /// Open the repository at `path`, typed, chosen in a file chooser or named by an action;
    /// answers why not when there is none.
    pub(super) fn open_repository_at(&mut self, path: &std::path::Path) -> Result<(), String> {
        if self.repository_busy() {
            return Err(crate::i18n::t("git-remote-busy"));
        }
        if self.bind_repository(path) {
            self.last_error = None;
            self.set_focus(None);
            self.screen = Screen::Changes;
            self.drawer.resolve(self.rect);
            // The record opens this repository from now on, on every device
            // that opens it.
            if let Some(path) = &self.repo_path {
                let path = path.to_string_lossy().into_owned();
                self.opened = Some(path.clone());
                let repository = crate::resource::GitClient::paths().repository;
                self.submit(Invoke::edit(&repository.set(ui::tilo_resource::Atom(path))));
            }
            Ok(())
        } else {
            let error = format!("No git repository at {}", path.display());
            self.last_error = Some(error.clone());
            Err(error)
        }
    }

    /// Give keyboard focus to one embedded text field (or none), keeping the
    /// toolkit inputs' focus flags mutually exclusive.
    pub(super) fn set_focus(&mut self, field: Option<Field>) {
        // The message is written in the list column, which a narrow window keeps in the drawer:
        // focusing the field opens it, so the person never types into something they cannot see.
        if field == Some(Field::Commit) && self.drawer.is_overlay() && !self.drawer.is_open() {
            // A window someone uses slides the column in; a capture puts it
            // where it stands, so the frame does not depend on the clock.
            if self.mode.is_live() {
                self.drawer.open();
            } else {
                self.drawer.snap(true);
            }
        }
        self.focus = field;
        self.commit_input.set_focus(field == Some(Field::Commit));
        self.branch_input.set_focus(field == Some(Field::Branch));
        self.open_input.set_focus(field == Some(Field::Open));
        self.worktree_branch_input
            .set_focus(field == Some(Field::WorktreeBranch));
        self.worktree_path_input
            .set_focus(field == Some(Field::WorktreePath));
        self.worktree_start_input
            .set_focus(field == Some(Field::WorktreeStart));
    }

    // The diff-view preferences are the app's only shared-document state. Each toggle updates the
    // local snapshot for an immediate redraw and dispatches its action through the framework's
    // local dispatch, which brackets the undo step and applies it through the executor, so it
    // records one undo step and replays identically on peers. These are the same actions the
    // resource tree exports.

    /// Flip the diff-pane layout, routed to by the window bar's layout items and the `d` shortcut.
    pub(super) fn toggle_diff_layout(&mut self) {
        if let Err(error) = self.switch_diff_layout() {
            self.last_error = Some(error);
        }
    }

    /// Write the shared repository projection when branches/commits/remotes move.
    pub(super) fn publish_shared_repo_if_needed(&mut self) {
        if self.awaiting_repo || !self.mode.is_live() {
            return;
        }
        let fingerprint = crate::shared::shared_fingerprint(&self.model);
        if self.shared_repo_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        let shared = crate::shared::SharedRepo::from_model(&self.model);
        self.submit(Ok(
            crate::resource::GitClient::RECORD_REPOSITORY.with(shared)
        ));
        self.shared_repo_fingerprint = Some(fingerprint);
    }

    pub(super) fn create_new_branch(&mut self) {
        if self.repository_busy() {
            return;
        }
        let name = self.branch_input.value().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::create_branch(&path, &name, None, true);
            let ok = res.is_ok();
            self.run_action(res);
            if ok {
                self.branch_input.set_text("");
                self.set_focus(None);
            }
        }
    }

    /// Stage or unstage a single hunk of the selected file, which the diff view assigns its button
    /// to. A staged file shows the HEAD→index diff, so its hunk action unstages, and an unstaged or
    /// untracked file shows the index→workdir diff, so its action stages.
    pub(super) fn stage_hunk(&mut self, hunk: usize) {
        if self.repository_busy() {
            return;
        }
        let Some((area, _)) = self.selected_change else {
            return;
        };
        if let (Some(path), Some(file)) = (self.repo(), self.current_path()) {
            let unstage = area == ChangeArea::Staged;
            // Map the displayed line's hunk to the Nth diff hunk for the file.
            let local = Self::local_hunk_index(&file, hunk);
            let res = crate::repo::apply_hunk(&path, &file, local, unstage);
            self.run_action(res);
            return;
        }
        // Sample fallback: move the whole file to prove the hit test.
        if let Some((area, idx)) = self.selected_change {
            self.toggle_stage_in_memory(area, idx);
        }
    }

    /// The 0-based hunk position within the file's diff: the parsed `DiffLine.hunk` already indexes
    /// the file's `DiffHunk` list, and this helper names the mapping.
    pub(super) fn local_hunk_index(file: &str, hunk: usize) -> usize {
        // `hunk` indexes `FileDiff.hunks`; libgit2's hunk callback walks the same hunks in the same
        // order, so they are 1:1.
        let _ = file;
        hunk
    }

    /// Apply a remote action against the live repository, then re-read the model so the
    /// ahead/behind counters reflect the real tracking state; for the sample, reconcile the
    /// counters so the capture shows the effect.
    pub(super) fn apply_remote_action(&mut self, action: RemoteAction) {
        if self.repo().is_some() {
            if let Err(error) = self.start_remote(action, None, None) {
                self.last_error = Some(error);
            }
            return;
        }
        if let Some(b) = self.model.branches.iter_mut().find(|b| b.is_head) {
            match action {
                RemoteAction::Fetch => {}
                RemoteAction::Pull => b.behind = 0,
                RemoteAction::Push => b.ahead = 0,
            }
        }
    }
}
