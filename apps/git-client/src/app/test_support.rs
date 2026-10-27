//! The drivers integration tests press: thin pass-throughs to the production action each one names, so a test drives the exact path the window does.

use super::*;

impl GitClientApp {
    // --- Test-facing drivers --------------------------------------------
    // These expose the *production* action methods so integration tests can
    // drive the exact code path the UI uses, then assert on the real repo.
    // They are thin pass-throughs — no test-only logic lives behind them.

    /// Drive the production "stage/unstage this file" action by path.
    #[doc(hidden)]
    pub fn test_toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        self.toggle_stage(area, idx);
    }
    /// Drive the production "stage all" action.
    #[doc(hidden)]
    pub fn test_stage_all(&mut self) {
        self.stage_all();
    }
    /// Set the commit message and drive the production commit action.
    #[doc(hidden)]
    pub fn test_commit(&mut self, message: &str) {
        self.commit_input.set_text(message);
        self.do_commit();
    }
    /// Drive the production per-hunk stage/unstage action for the selected file.
    #[doc(hidden)]
    pub fn test_stage_hunk(&mut self, hunk: usize) {
        self.stage_hunk(hunk);
    }
    /// Drive the production branch action (checkout / delete / stash-apply).
    #[doc(hidden)]
    pub fn test_branch_action(&mut self, action: BranchAction) {
        self.apply_branch_action(action);
    }
    /// Drive the production "create branch" action from a name.
    #[doc(hidden)]
    pub fn test_create_branch(&mut self, name: &str) {
        self.branch_input.set_text(name);
        self.create_new_branch();
    }
    /// Drive the production "discard file" action by area + index.
    #[doc(hidden)]
    pub fn test_discard_file(&mut self, area: ChangeArea, idx: usize) {
        self.discard_file(area, idx);
    }
    /// Whether the app is showing the empty "open a repository" state.
    #[doc(hidden)]
    pub fn test_awaiting_repo(&self) -> bool {
        self.awaiting_repo
    }
    /// Drive the empty-state "open this path" action.
    #[doc(hidden)]
    pub fn test_open_path(&mut self, path: &str) {
        self.open_input.set_text(path);
        self.open_repo_from_input();
    }
    /// Select a file in a given area + index (mirrors a list click).
    #[doc(hidden)]
    pub fn test_select(&mut self, area: ChangeArea, idx: usize) {
        self.selected_change = Some((area, idx));
    }
    /// The current `head` shorthand from the live model.
    #[doc(hidden)]
    pub fn test_head(&self) -> &str {
        &self.model.head
    }
    /// Counts of (staged, unstaged, untracked) entries in the live model.
    #[doc(hidden)]
    pub fn test_change_counts(&self) -> (usize, usize, usize) {
        (
            self.model.staged.len(),
            self.model.unstaged.len(),
            self.model.untracked.len(),
        )
    }
    /// The live model's commit summaries, newest first.
    #[doc(hidden)]
    pub fn test_commit_summaries(&self) -> Vec<String> {
        self.model
            .commits
            .iter()
            .map(|c| c.summary.clone())
            .collect()
    }
    /// The live model's local + remote branch names.
    #[doc(hidden)]
    pub fn test_branch_names(&self) -> Vec<String> {
        self.model.branches.iter().map(|b| b.name.clone()).collect()
    }
    /// The last action error message, if any.
    #[doc(hidden)]
    pub fn test_last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
    /// What the last action did, as the banner says it.
    #[doc(hidden)]
    pub fn test_notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    /// The branch whose switch is waiting for an answer, if any.
    #[doc(hidden)]
    pub fn test_pending_switch(&self) -> Option<&str> {
        match &self.confirm {
            Some(Confirm::SwitchBranch { name, .. }) => Some(name.as_str()),
            _ => None,
        }
    }
    /// Answer the pending switch the way its sheet's buttons do.
    #[doc(hidden)]
    pub fn test_answer_switch(&mut self, plan: crate::repo::CheckoutPlan) {
        let choice = match plan {
            crate::repo::CheckoutPlan::Carry => ConfirmChoice::SwitchCarrying,
            crate::repo::CheckoutPlan::Stash => ConfirmChoice::SwitchStashing,
        };
        self.perform_confirmed(choice);
    }
    /// Show commit `index` and read its changed files, as opening History does.
    #[doc(hidden)]
    pub fn test_commit_files(&mut self, index: usize) -> Vec<String> {
        self.select_commit(index);
        let Some(commit) = self.model.commits.get(index) else {
            return Vec::new();
        };
        let id = commit.id.clone();
        self.load_commit_files(&id);
        self.commit_files
            .get(&id)
            .map(|files| files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default()
    }
    /// Open the diff of file `index` in the selected commit.
    #[doc(hidden)]
    pub fn test_open_commit_file(&mut self, index: usize) {
        self.open_commit_file(index);
    }
    /// The commit diff on screen, as `(commit, file)`.
    #[doc(hidden)]
    pub fn test_commit_diff(&self) -> Option<(String, String)> {
        self.commit_diff.clone()
    }
    /// Select the first conflicted file, as choosing its row does.
    #[doc(hidden)]
    pub fn test_select_conflicted_file(&mut self) {
        for area in [
            crate::model::ChangeArea::Unstaged,
            crate::model::ChangeArea::Staged,
            crate::model::ChangeArea::Untracked,
        ] {
            let found = change_list(&self.model, area)
                .iter()
                .position(|file| file.kind == crate::model::ChangeKind::Conflicted);
            if let Some(index) = found {
                self.selected_change = Some((area, index));
                return;
            }
        }
    }
    /// Keep one side of the selected conflicted file, as `o` and `t` do.
    #[doc(hidden)]
    pub fn test_resolve_conflict(&mut self, side: crate::repo::ConflictSide) {
        self.resolve_conflict(side);
    }
}
