//! What the selection becomes: the diff a row names, staging and unstaging a file or a hunk, the commit, discard, and the branch verbs.

use super::*;

impl GitClientApp {
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

    /// Stage or unstage a single file. Against a real repo this calls
    /// `git2` (`add` / `reset HEAD -- path`) and re-reads the model so the
    /// list reflects the actual index. For the sample it falls back to the
    /// in-memory move so the offscreen capture still demonstrates the flow.
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

    /// In-memory move used only by the sample-backed app (no repo on disk).
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

    /// Sample-only commit: synthesise a tip commit so the offscreen capture
    /// shows the action's effect without a repository.
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

    /// Discard a file's worktree changes (revert unstaged edits, or delete an
    /// untracked file). The controls reach here only through the
    /// confirmation sheet.
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
        // Only worktree (unstaged/untracked) hunks are discardable — a staged
        // file's diff is the index side, which "discard" doesn't act on.
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
        let raw = self.open_input.value().trim().to_string();
        let _ = self.open_repository_at(&raw);
    }

    /// Open the repository at `raw`, typed, chosen in the file chooser or
    /// named by an action. Answers why not when there is none.
    pub(super) fn open_repository_at(&mut self, raw: &str) -> Result<(), String> {
        if self.repository_busy() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("no repository path was given".into());
        }
        if self.bind_repository(std::path::Path::new(raw)) {
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
            let error = format!("No git repository at {raw}");
            self.last_error = Some(error.clone());
            Err(error)
        }
    }

    /// Give keyboard focus to one embedded text field (or none), keeping the
    /// toolkit inputs' focus flags mutually exclusive.
    pub(super) fn set_focus(&mut self, field: Option<Field>) {
        // The message is written in the list column, which a narrow window
        // keeps in the drawer: focusing the field opens it, so the person is
        // never typing into something they cannot see.
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

    // --- Durable view-preference actions -------------------------------
    // The diff-view preferences are the app's only shared-document state.
    // Each toggle updates the local snapshot for an immediate redraw AND
    // dispatches its `GitClientAction` through the framework `ActionHost`,
    // which brackets the undo boundary and applies it through the executor
    // (so it records one Ctrl+Z step and replays byte-identically on peers).
    // The next `sync` re-reads the same value straight back out of the doc.
    // These are the exact actions the Ctrl+K resource tree exports — one
    // source of truth (see `crate::actions`).

    /// Flip the diff-pane layout (unified ⇄ side-by-side). Routed to by the
    /// window bar's layout items and the `d` shortcut.
    pub(super) fn toggle_diff_layout(&mut self) {
        if let Err(error) = self.switch_diff_layout() {
            self.last_error = Some(error);
        }
    }

    /// Flip diff syntax highlighting. Routed to by the window bar's item.
    pub(super) fn toggle_syntax_highlight(&mut self) {
        if let Err(error) = self.change_preferences(|s| s.syntax_highlight = !s.syntax_highlight) {
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

    /// Stage (or unstage) a single hunk of the selected file. Against a real
    /// repo this applies exactly that hunk to the index via `git2` (the
    /// equivalent of `git add -p` / `git restore --staged -p`) and re-reads.
    /// The `hunk` argument is the hunk's index within the file's diff, which
    /// the diff view assigns its button to.
    ///
    /// The currently shown diff direction depends on which area is selected:
    /// a staged file shows the HEAD→index diff (so a hunk action *unstages*),
    /// an unstaged/untracked file shows the index→workdir diff (so a hunk
    /// action *stages*). That matches `apply_hunk`'s ordering exactly.
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
        // Sample fallback: move the whole file to prove the hit-test.
        if let Some((area, idx)) = self.selected_change {
            self.toggle_stage_in_memory(area, idx);
        }
    }

    /// The 0-based hunk position (within the file's diff, excluding meta
    /// "hunk 0 = file header" noise) for the diff line's `hunk` field. The
    /// parsed `DiffLine.hunk` is already a 0-based index into the file's
    /// `DiffHunk` list, so it maps directly; this helper exists so the
    /// mapping is named and testable if the parser's convention shifts.
    pub(super) fn local_hunk_index(file: &str, hunk: usize) -> usize {
        // `hunk` indexes `FileDiff.hunks`; libgit2's hunk callback walks the
        // same hunks in the same order, so they're 1:1.
        let _ = file;
        hunk
    }

    /// Apply a remote action (fetch / pull / push) against the live repo via
    /// `git2`, then re-read the model so the ahead/behind counters reflect
    /// the real tracking state. For the sample, reconcile the counters so the
    /// offscreen capture shows the effect.
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
