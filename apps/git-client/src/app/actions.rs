//! What a press, a key or an assistive action does: the one `Hit` path they share, the confirmation and sheet answers, the screen switch and the branch switch.

use super::*;

impl GitClientApp {
    /// Apply a branch action (checkout / create / delete) or a stash apply
    /// against the live repo, then re-read. Sample-backed apps ignore these
    /// (the panels' branch buttons only appear for real repos).
    pub(super) fn apply_branch_action(&mut self, action: BranchAction) {
        if self.repository_busy() {
            return;
        }
        let Some(path) = self.repo() else { return };
        let res = match action {
            // Switching branches with work in the tree asks first: the work
            // either comes along or is parked, and the person decides which.
            BranchAction::Checkout(name) => {
                if self.model.change_count() > 0 {
                    self.confirm = Some(Confirm::SwitchBranch { name });
                    return;
                }
                crate::repo::checkout_branch(&path, &name)
            }
            BranchAction::Delete(name) => crate::repo::delete_branch(&path, &name),
            BranchAction::StashApply(i) => crate::repo::stash_apply(&path, i),
        };
        self.run_action(res);
    }

    pub(super) fn current_path(&self) -> Option<String> {
        let (area, idx) = self.selected_change?;
        change_list(&self.model, area)
            .get(idx)
            .map(|f| f.path.clone())
    }

    /// Whether an open drawer covers the window: everything under its scrim
    /// is inert until it closes.
    pub(super) fn drawer_is_modal(&self) -> bool {
        self.screen.has_list() && self.drawer.is_overlay() && self.drawer.is_open()
    }

    pub(super) fn push_target(&mut self, rect: Rect, hit: Hit) {
        if rect.width() > 0.0 && rect.height() > 0.0 {
            self.targets.push(Target {
                rect,
                hit: hit.clone(),
            });
            self.semantic.push(Target { rect, hit });
        }
    }

    /// Do what `hit` names: the one path pointer presses, keys and
    /// assistive actions share.
    pub(super) fn activate(&mut self, hit: Hit, _doc: &mut ui::widget::Edits<'_>) {
        if let (Some(dispatcher), Some((row, effect))) =
            (&self.dispatcher, self.effect_for_hit(&hit))
        {
            if let Some(row) = row {
                dispatcher.effect_on(&row, &effect);
            } else {
                dispatcher.effect(&effect);
            }
            return;
        }
        let in_drawer = self.drawer_is_modal();
        match hit {
            Hit::Scrim => {
                if !self
                    .pointer
                    .is_some_and(|point| self.drawer.sidebar_rect().contains(point))
                {
                    self.drawer.close();
                }
            }
            Hit::DismissError => {
                self.last_error = None;
                self.notice = None;
            }
            Hit::Change(change) => self.activate_change(change, in_drawer),
            Hit::Diff(diff) => self.activate_diff(diff),
            Hit::Commit(index) => {
                self.select_commit(index);
                if in_drawer {
                    self.drawer.close();
                }
            }
            Hit::CommitFile(index) => {
                self.file_selection = Some(index);
                self.open_commit_file(index);
            }
            Hit::BackToCommit => {
                self.commit_diff = None;
                self.diff_scroll = 0.0;
                self.diff_hscroll = 0.0;
            }
            Hit::ResolveConflict(side) => self.resolve_conflict(side),
            // Read or asked, never pressed: the conflict's own sentence, the
            // question a sheet puts, and the repository chooser, which is
            // dispatched as its effect.
            Hit::ConflictNote | Hit::ConfirmQuestion | Hit::ChooseRepository => {}
            // The sheet's answers, reached by a press or by assistive
            // technology activating the button it names.
            Hit::ConfirmAnswer(index) => self.answer_confirm(index),
            Hit::Panel(panel) => self.activate_panel(panel),
            Hit::OpenPath => self.set_focus(Some(Field::Open)),
            Hit::OpenRepository => self.open_repo_from_input(),
            // Dispatched as its effect (`effect_for_hit`); a direct press
            // without the published action does nothing.
        }
    }

    pub(super) fn activate_change(&mut self, hit: ChangesHit, in_drawer: bool) {
        match hit {
            ChangesHit::SelectFile(area, idx) => {
                self.selected_change = Some((area, idx));
                self.diff_scroll = 0.0;
                self.diff_hscroll = 0.0;
                // Choosing in a drawer shows what was chosen.
                if in_drawer {
                    self.drawer.close();
                }
            }
            ChangesHit::ToggleStage(area, idx) => self.toggle_stage(area, idx),
            ChangesHit::StageAll(_) => self.stage_all(),
            ChangesHit::UnstageAll(_) => self.unstage_all(),
            ChangesHit::Commit => self.do_commit(),
            ChangesHit::FocusMessage => self.set_focus(Some(Field::Commit)),
        }
    }

    pub(super) fn activate_diff(&mut self, hit: DiffHit) {
        let Some((area, index)) = self.selected_change else {
            return;
        };
        match hit {
            DiffHit::StageFile => self.toggle_stage(area, index),
            DiffHit::DiscardFile => {
                if let Some(file) = change_list(&self.model, area).get(index) {
                    self.confirm = Some(Confirm::DiscardFile {
                        area,
                        index,
                        name: file.name().to_string(),
                    });
                }
            }
            DiffHit::Hunk(hunk, HunkAction::Toggle) => self.stage_hunk(hunk),
            DiffHit::Hunk(hunk, HunkAction::Discard) => {
                self.confirm = Some(Confirm::DiscardHunk(hunk));
            }
        }
    }

    pub(super) fn activate_panel(&mut self, hit: PanelHit) {
        match hit {
            PanelHit::Branch(BranchAction::Delete(name)) => {
                self.confirm = Some(Confirm::DeleteBranch(name));
            }
            PanelHit::Branch(action) => self.apply_branch_action(action),
            PanelHit::BranchName => self.set_focus(Some(Field::Branch)),
            PanelHit::CreateBranch => self.create_new_branch(),
            PanelHit::WorktreeBranch => self.set_focus(Some(Field::WorktreeBranch)),
            PanelHit::WorktreePath => self.set_focus(Some(Field::WorktreePath)),
            PanelHit::WorktreeStart => self.set_focus(Some(Field::WorktreeStart)),
            PanelHit::WorktreeInfo(_) | PanelHit::WorktreeError => {}
            PanelHit::CreateWorktree => {
                self.last_error = Some(t("git-worktree-host-required"));
            }
        }
    }

    /// Carry out the answer `choice` to the open confirmation sheet.
    pub(super) fn perform_confirmed(&mut self, choice: ConfirmChoice) {
        match (self.confirm.take(), choice) {
            (Some(Confirm::DiscardFile { area, index, .. }), ConfirmChoice::Discard) => {
                self.discard_file(area, index);
            }
            (Some(Confirm::DiscardHunk(hunk)), ConfirmChoice::Discard) => {
                self.discard_hunk(hunk);
            }
            (Some(Confirm::DeleteBranch(name)), ConfirmChoice::DeleteBranch) => {
                self.apply_branch_action(BranchAction::Delete(name));
            }
            (Some(Confirm::SwitchBranch { name, .. }), ConfirmChoice::SwitchCarrying) => {
                self.switch_branch(&name, crate::repo::CheckoutPlan::Carry);
            }
            (Some(Confirm::SwitchBranch { name, .. }), ConfirmChoice::SwitchStashing) => {
                self.switch_branch(&name, crate::repo::CheckoutPlan::Stash);
            }
            _ => {}
        }
    }

    /// Switch to `name`, deciding what happens to uncommitted work through
    /// `plan`, and say what became of it.
    pub(super) fn switch_branch(&mut self, name: &str, plan: crate::repo::CheckoutPlan) {
        let Some(path) = self.repo() else { return };
        match crate::repo::checkout_branch_planned(&path, name, plan) {
            Ok(stash) => {
                self.last_error = None;
                self.notice = Some(match stash {
                    Some(stash) => t_args(
                        "git-switched-stashed",
                        &[("name", name.into()), ("stash", stash.into())],
                    ),
                    None => t_args("git-switched", &[("name", name.into())]),
                });
                self.reload();
            }
            Err(error) => self.report_error(&error),
        }
    }

    /// Answer the sheet with its action `index`: Cancel (0) puts the
    /// question away, and the rest are its own answers in the order it
    /// offered them.
    pub(super) fn answer_confirm(&mut self, index: usize) {
        let choice = match index {
            0 => None,
            index => self.confirm.as_ref().and_then(|confirm| {
                confirm
                    .choices()
                    .get(index.saturating_sub(1))
                    .map(|face| face.choice)
            }),
        };
        self.confirm_sheet.reset();
        match choice {
            // `perform_confirmed` takes the question; leaving it standing
            // where no answer was found puts it away instead of asking
            // forever.
            Some(choice) => self.perform_confirmed(choice),
            None => self.confirm = None,
        }
    }

    /// A press, a key or an assistive activation while the sheet is up: the
    /// sheet's own answers, its close control and its scrim; nothing under
    /// it.
    pub(super) fn answer_sheet(&mut self, response: ui::widget::SheetResponse) {
        match response {
            ui::widget::SheetResponse::Action(index) => self.answer_confirm(index),
            ui::widget::SheetResponse::Dismiss => {
                self.confirm = None;
                self.confirm_sheet.reset();
            }
        }
    }
}
