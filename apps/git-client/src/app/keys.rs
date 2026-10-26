//! The keyboard: every decision the client can make, without the pointer.
//!
//! The lists move with the arrow keys; this module is the part that *does*
//! something — stage a file, write a message, keep a side of a conflict,
//! check out a branch. Each key is one letter with no chord, and the same
//! letter means the same thing on every screen it applies to.

use winit::keyboard::{Key, NamedKey};

use crate::panels::BranchAction;

use super::{Confirm, Field, GitClientApp, Screen};

impl GitClientApp {
    /// The letters that do the screen's work, so a file can be staged, a
    /// message written, a branch checked out and a conflict settled without
    /// the pointer. Each is one key with no chord: the same letter does the
    /// same thing on every screen it makes sense on.
    ///
    /// * `s` stage or unstage the selected file, `S` (Shift) every file
    /// * `o`/`t` keep our or their side of a conflicted file
    /// * `c` write the commit message, `Ctrl+Enter` commits from anywhere
    /// * `b` writes a new branch name on the branches page
    /// * `Enter` opens the selected commit's first file, or checks out the
    ///   selected branch; `Backspace` steps back out of a commit file's diff
    /// * `Delete` deletes the selected branch, after the same confirmation
    pub(super) fn handle_action_key(
        &mut self,
        key: &str,
        modifiers: winit::event::Modifiers,
        logical: &Key,
    ) -> bool {
        let command = self.conventions.command_modifier_down(modifiers.state());
        if command {
            if matches!(logical, Key::Named(NamedKey::Enter)) && self.screen == Screen::Changes {
                self.do_commit();
                return true;
            }
            return false;
        }
        match (self.screen, key) {
            (Screen::Changes, "s") => {
                if let Some((area, index)) = self.selected_change {
                    self.toggle_stage(area, index);
                    return true;
                }
                false
            }
            (Screen::Changes, "S") => {
                self.stage_all();
                true
            }
            (Screen::Changes, "o") => {
                if self.selected_conflict().is_some() {
                    self.resolve_conflict(crate::repo::ConflictSide::Ours);
                    return true;
                }
                false
            }
            (Screen::Changes, "t") => {
                if self.selected_conflict().is_some() {
                    self.resolve_conflict(crate::repo::ConflictSide::Theirs);
                    return true;
                }
                false
            }
            (Screen::Changes, "c") => {
                self.set_focus(Some(Field::Commit));
                true
            }
            (Screen::Panels, "b") => {
                self.set_focus(Some(Field::Branch));
                true
            }
            _ => false,
        }
    }

    /// Keys that act on the file list of the selected commit, and the branch
    /// list: `Enter` opens what is under the keyboard, `Backspace` steps
    /// back, `Delete` deletes the selected branch.
    pub(super) fn navigate_panels(&mut self, key: &Key) -> bool {
        match (self.screen, key) {
            (Screen::History, Key::Named(NamedKey::Enter)) => {
                if self.commit_diff.is_none() {
                    let index = self.file_selection.unwrap_or(0);
                    self.open_commit_file(index);
                }
                true
            }
            (Screen::History, Key::Named(NamedKey::Backspace | NamedKey::Escape)) => {
                if self.commit_diff.take().is_some() {
                    self.diff_scroll = 0.0;
                    self.diff_hscroll = 0.0;
                    return true;
                }
                false
            }
            (Screen::Panels, Key::Named(NamedKey::Enter)) => {
                if let Some(name) = self.selected_branch_name() {
                    self.apply_branch_action(BranchAction::Checkout(name));
                    return true;
                }
                false
            }
            (Screen::Panels, Key::Named(NamedKey::Delete)) => {
                if let Some(name) = self.selected_branch_name() {
                    self.confirm = Some(Confirm::DeleteBranch(name));
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// The local branch under the keyboard on the branches page.
    pub(super) fn selected_branch_name(&self) -> Option<String> {
        if self.screen != Screen::Panels {
            return None;
        }
        let index = self.branch_selection?;
        self.model
            .branches
            .iter()
            .filter(|branch| !branch.is_remote)
            .nth(index)
            .map(|branch| branch.name.clone())
    }

    /// Move the branches page's selection, skipping nothing: every local
    /// branch is one arrow away from a checkout.
    pub(super) fn navigate_panels_selection(&mut self, step: isize) -> bool {
        let local = self
            .model
            .branches
            .iter()
            .filter(|branch| !branch.is_remote)
            .count();
        if local == 0 {
            return false;
        }
        let current = self
            .branch_selection
            .unwrap_or(0)
            .min(local.saturating_sub(1));
        let next = current
            .saturating_add_signed(step)
            .min(local.saturating_sub(1));
        self.branch_selection = Some(next);
        // Keep the chosen branch in view as the selection moves.
        self.panels_scroll = self.panels_scroll.max(0.0);
        true
    }

    /// Arrow keys move the selection in the active list. Returns whether
    /// `key` did.
    pub(super) fn navigate_list(&mut self, key: &Key) -> bool {
        let step = match key {
            Key::Named(NamedKey::ArrowDown) => 1,
            Key::Named(NamedKey::ArrowUp) => -1,
            _ => return false,
        };
        match self.screen {
            Screen::History => {
                if self.commit_diff.is_none() && self.file_view_focused() {
                    // Inside a commit's file list the arrows move between its
                    // files, so Enter opens the one under the keyboard.
                    let count = self.commit_file_count();
                    if count > 0 {
                        let current = self.file_selection.unwrap_or(0).min(count - 1);
                        let next = current
                            .saturating_add_signed(step)
                            .min(count.saturating_sub(1));
                        self.file_selection = Some(next);
                        return true;
                    }
                }
                let next = if step > 0 {
                    self.selected_commit
                        .saturating_add(1)
                        .min(self.model.commits.len().saturating_sub(1))
                } else {
                    self.selected_commit.saturating_sub(1)
                };
                self.select_commit(next);
                true
            }
            Screen::Changes => {
                self.select_next_change(i32::try_from(step).unwrap_or(0));
                true
            }
            Screen::Panels => self.navigate_panels_selection(step),
        }
    }

    /// Whether the keyboard is in the selected commit's file list (it is,
    /// once an arrow has chosen a file and no diff is open).
    pub(super) fn file_view_focused(&self) -> bool {
        self.file_selection.is_some()
    }

    /// How many changed files the selected commit has, once they are read.
    pub(super) fn commit_file_count(&self) -> usize {
        self.model
            .commits
            .get(self.selected_commit)
            .and_then(|commit| self.commit_files.get(&commit.id))
            .map_or(0, Vec::len)
    }
}
