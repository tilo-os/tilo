//! The drivers integration tests press: thin pass-throughs to the production action each one names, so a test drives the exact path the window does.

use super::{BranchAction, ChangeArea, Confirm, ConfirmChoice, GitClientApp};

impl GitClientApp {
    // --- Test-facing drivers --------------------------------------------
    // These expose the *production* action methods so integration tests can
    // drive the exact code path the UI uses, then assert on the real repo.
    // They are thin pass-throughs — no test-only logic lives behind them.

    /// Drive the production "stage/unstage this file" action by path.
    #[doc(hidden)]
    pub fn test_toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        self.state().toggle_stage(area, idx);
    }
    /// Drive the production "stage all" action.
    #[doc(hidden)]
    pub fn test_stage_all(&mut self) {
        self.state().stage_all();
    }
    /// Set the commit message and drive the production commit action.
    #[doc(hidden)]
    pub fn test_commit(&mut self, message: &str) {
        let mut state = self.state();
        state.commit_input.set_text(message);
        state.do_commit();
    }
    /// Drive the production per-hunk stage/unstage action for the selected file.
    #[doc(hidden)]
    pub fn test_stage_hunk(&mut self, hunk: usize) {
        self.state().stage_hunk(hunk);
    }
    /// Drive the production branch action (checkout / delete / stash-apply).
    #[doc(hidden)]
    pub fn test_branch_action(&mut self, action: BranchAction) {
        self.state().apply_branch_action(action);
    }
    /// Drive the production "create branch" action from a name.
    #[doc(hidden)]
    pub fn test_create_branch(&mut self, name: &str) {
        let mut state = self.state();
        state.branch_input.set_text(name);
        state.create_new_branch();
    }
    /// Drive the production "discard file" action by area + index.
    #[doc(hidden)]
    pub fn test_discard_file(&mut self, area: ChangeArea, idx: usize) {
        self.state().discard_file(area, idx);
    }
    /// Whether the app is showing the empty "open a repository" state.
    #[doc(hidden)]
    pub fn test_awaiting_repo(&self) -> bool {
        self.state().awaiting_repo
    }
    /// Drive the empty-state "open this path" action.
    #[doc(hidden)]
    pub fn test_open_path(&mut self, path: &str) {
        let mut state = self.state();
        state.open_input.set_text(path);
        state.open_repo_from_input();
    }
    /// Select a file in a given area + index (mirrors a list click).
    #[doc(hidden)]
    pub fn test_select(&mut self, area: ChangeArea, idx: usize) {
        self.state().selected_change = Some((area, idx));
    }
    /// The current `head` shorthand from the live model.
    #[doc(hidden)]
    pub fn test_head(&self) -> String {
        self.state().model.head.clone()
    }
    /// Counts of (staged, unstaged, untracked) entries in the live model.
    #[doc(hidden)]
    pub fn test_change_counts(&self) -> (usize, usize, usize) {
        let state = self.state();
        (
            state.model.staged.len(),
            state.model.unstaged.len(),
            state.model.untracked.len(),
        )
    }
    /// The live model's commit summaries, newest first.
    #[doc(hidden)]
    pub fn test_commit_summaries(&self) -> Vec<String> {
        self.state()
            .model
            .commits
            .iter()
            .map(|c| c.summary.clone())
            .collect()
    }
    /// The live model's local + remote branch names.
    #[doc(hidden)]
    pub fn test_branch_names(&self) -> Vec<String> {
        self.state()
            .model
            .branches
            .iter()
            .map(|b| b.name.clone())
            .collect()
    }
    /// The last action error message, if any.
    #[doc(hidden)]
    pub fn test_last_error(&self) -> Option<String> {
        self.state().last_error.clone()
    }
    /// What the last action did, as the banner says it.
    #[doc(hidden)]
    pub fn test_notice(&self) -> Option<String> {
        self.state().notice.clone()
    }
    /// The branch whose switch is waiting for an answer, if any.
    #[doc(hidden)]
    pub fn test_pending_switch(&self) -> Option<String> {
        match &self.state().confirm {
            Some(Confirm::SwitchBranch { name, .. }) => Some(name.clone()),
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
        self.state().perform_confirmed(choice);
    }
    /// Show commit `index` and read its changed files, as opening History does.
    #[doc(hidden)]
    pub fn test_commit_files(&mut self, index: usize) -> Vec<String> {
        let mut state = self.state();
        state.select_commit(index);
        let Some(commit) = state.model.commits.get(index) else {
            return Vec::new();
        };
        let id = commit.id.clone();
        state.load_commit_files(&id);
        state
            .commit_files
            .get(&id)
            .map(|files| files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default()
    }
    /// Open the diff of file `index` in the selected commit.
    #[doc(hidden)]
    pub fn test_open_commit_file(&mut self, index: usize) {
        self.state().open_commit_file(index);
    }
    /// The commit diff on screen, as `(commit, file)`.
    #[doc(hidden)]
    pub fn test_commit_diff(&self) -> Option<(String, String)> {
        self.state().commit_diff.clone()
    }
    /// Select the first conflicted file, as choosing its row does.
    #[doc(hidden)]
    pub fn test_select_conflicted_file(&mut self) {
        self.state().select_conflicted_file();
    }
    /// Keep one side of the selected conflicted file, as `o` and `t` do.
    #[doc(hidden)]
    pub fn test_resolve_conflict(&mut self, side: crate::repo::ConflictSide) {
        self.state().resolve_conflict(side);
    }
}
