//! What assistive technology addresses: the targets the last paint recorded, the semantic hit they resolve to, the entries published for a reader and the tree a change becomes.

use super::*;

impl GitClientApp {
    /// The semantic inventory of the last paint: one node per control and
    /// row, keyed by a stable action target.
    pub(super) fn accessibility_targets(&self) -> Vec<(String, Rect, ui::a11y::AccessNode)> {
        self.semantic_entries()
            .into_iter()
            .map(|(target, rect, node, _)| (target, rect, node))
            .collect()
    }

    /// What the semantic target `target` does, if it does anything.
    pub(super) fn semantic_hit(&self, target: &str) -> Option<Hit> {
        self.semantic_entries()
            .into_iter()
            .find(|(candidate, _, _, _)| candidate == target)
            .and_then(|(_, _, _, hit)| hit)
    }

    /// The semantic inventory with what each entry does.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive match keeps every kind of target and its node auditable together"
    )]
    pub(super) fn semantic_entries(
        &self,
    ) -> Vec<(String, Rect, ui::a11y::AccessNode, Option<Hit>)> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let button = |target: String, rect: Rect, label: String| {
            (
                target,
                rect,
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect),
            )
        };
        let field = |target: &str, rect: Rect, input: &TextInput, name: &str| {
            (
                target.to_string(),
                rect,
                input
                    .accessibility()
                    .unwrap_or_else(|| AccessNode::new(Role::TextInput).with_name(name))
                    .with_bounds(rect),
            )
        };
        let mut out = Vec::new();
        for target in &self.semantic {
            let rect = target.rect;
            let entry = match &target.hit {
                Hit::DismissError => {
                    let text = if let Some(job) = &self.remote_job {
                        job.label.clone()
                    } else {
                        self.last_error.clone().unwrap_or_default()
                    };
                    out.push((
                        "git.status".to_string(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(&text)
                            .with_value(&text)
                            .with_live(ui::a11y::Politeness::Polite)
                            .with_bounds(rect),
                        None,
                    ));
                    if self.remote_job.is_some() {
                        continue;
                    }
                    button("git.dismiss-error".into(), rect, t("git-dismiss-error"))
                }
                Hit::Change(ChangesHit::FocusMessage) => field(
                    "git.commit-message",
                    rect,
                    &self.commit_input,
                    &t("git-commit-message"),
                ),
                Hit::Change(hit) => match self.change_node(hit, rect) {
                    Some(entry) => entry,
                    None => continue,
                },
                Hit::Diff(hit) => {
                    let (target, name) = match hit {
                        DiffHit::StageFile if self.selected_is_staged() => {
                            ("git.diff.unstage-file".to_string(), t("git-unstage-file"))
                        }
                        DiffHit::StageFile => {
                            ("git.diff.stage-file".to_string(), t("git-stage-file"))
                        }
                        DiffHit::DiscardFile => {
                            ("git.discard-file".to_string(), t("git-discard-file-name"))
                        }
                        DiffHit::Hunk(hunk, action) => {
                            let name = t(match action {
                                HunkAction::Toggle if self.selected_is_staged() => {
                                    "git-unstage-hunk"
                                }
                                HunkAction::Toggle => "git-stage-hunk",
                                HunkAction::Discard => "git-discard-hunk",
                            });
                            (format!("git.hunk:{hunk}:{action:?}"), name)
                        }
                    };
                    button(target, rect, name)
                }
                Hit::Commit(index) => {
                    let Some(commit) = self.model.commits.get(*index) else {
                        continue;
                    };
                    (
                        format!("git.commit:{}", commit.id),
                        rect,
                        AccessNode::new(Role::ListItem)
                            .with_name(t_args(
                                "git-commit-row-name",
                                &[
                                    ("summary", commit.summary.as_str().into()),
                                    ("author", commit.author_name.as_str().into()),
                                    ("age", history::age(commit.time).into()),
                                ],
                            ))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(*index == self.selected_commit),
                            )
                            .with_bounds(rect),
                    )
                }
                Hit::CommitFile(index) => {
                    let commit_id = self
                        .model
                        .commits
                        .get(self.selected_commit)
                        .map(|commit| commit.id.clone())
                        .unwrap_or_default();
                    let Some(file) = self
                        .commit_files
                        .get(&commit_id)
                        .and_then(|files| files.get(*index))
                    else {
                        continue;
                    };
                    (
                        format!("git.commit-file:{index}"),
                        rect,
                        AccessNode::new(Role::ListItem)
                            .with_name(t_args(
                                "git-commit-file-name",
                                &[
                                    ("file", file.path.as_str().into()),
                                    ("kind", file.kind.label().into()),
                                ],
                            ))
                            .with_description(t("git-show-commit-file"))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(self.file_selection == Some(*index)),
                            )
                            .with_bounds(rect),
                    )
                }
                Hit::BackToCommit => {
                    button("git.back-to-commit".into(), rect, t("git-back-to-commit"))
                }
                Hit::ConflictNote => {
                    let note = self.conflict_sentence();
                    out.push((
                        "git.conflict.note".into(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(&note)
                            .with_value(&note)
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                // The question and what answering costs, read out as one
                // sentence: a reader hears what it is deciding about.
                Hit::ConfirmQuestion => {
                    let Some(confirm) = &self.confirm else {
                        continue;
                    };
                    let text = format!("{} {}", confirm.title(), confirm.body());
                    out.push((
                        "git.confirm.question".into(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(&text)
                            .with_value(&text)
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                Hit::ConfirmAnswer(index) => {
                    let Some(confirm) = &self.confirm else {
                        continue;
                    };
                    let name = match *index {
                        0 => t("git-cancel"),
                        index => match confirm.choices().get(index.saturating_sub(1)) {
                            Some(face) => face.label.clone(),
                            None => continue,
                        },
                    };
                    out.push((
                        format!("git.confirm:{index}"),
                        rect,
                        AccessNode::new(Role::Button)
                            .with_name(&name)
                            .with_state(AccessState::new().focusable(true))
                            .with_bounds(rect),
                        Some(Hit::ConfirmAnswer(*index)),
                    ));
                    continue;
                }
                Hit::ResolveConflict(side) => {
                    let (target, name) = match side {
                        crate::repo::ConflictSide::Ours => {
                            ("git.conflict.ours".to_string(), t("git-conflict-take-ours"))
                        }
                        crate::repo::ConflictSide::Theirs => (
                            "git.conflict.theirs".to_string(),
                            t("git-conflict-take-theirs"),
                        ),
                    };
                    button(target, rect, name)
                }
                Hit::Panel(PanelHit::WorktreeInfo(index)) => {
                    let Some(worktree) = self.model.worktrees.get(*index) else {
                        continue;
                    };
                    out.push((
                        format!("git.worktree:{}", worktree.path),
                        rect,
                        AccessNode::new(Role::ListItem)
                            .with_name(worktree.head.clone())
                            .with_description(worktree.path.clone())
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                Hit::Panel(PanelHit::WorktreeError) => {
                    let Some(error) = &self.model.worktree_error else {
                        continue;
                    };
                    out.push((
                        "git.worktrees-error".into(),
                        rect,
                        AccessNode::new(Role::StaticText)
                            .with_name(t("git-worktree-read-failed"))
                            .with_description(error.clone())
                            .with_bounds(rect),
                        None,
                    ));
                    continue;
                }
                Hit::Panel(PanelHit::BranchName) => field(
                    "git.new-branch-name",
                    rect,
                    &self.branch_input,
                    &t("git-new-branch-name"),
                ),
                Hit::Panel(PanelHit::WorktreeBranch) => field(
                    "git.worktree-branch",
                    rect,
                    &self.worktree_branch_input,
                    &t("git-worktree-branch"),
                ),
                Hit::Panel(PanelHit::WorktreePath) => field(
                    "git.worktree-path",
                    rect,
                    &self.worktree_path_input,
                    &t("git-worktree-path"),
                ),
                Hit::Panel(PanelHit::WorktreeStart) => field(
                    "git.worktree-start",
                    rect,
                    &self.worktree_start_input,
                    &t("git-worktree-start"),
                ),
                Hit::Panel(PanelHit::CreateWorktree) => {
                    let (target, rect, node) =
                        button("git.create-worktree".into(), rect, t("git-create-worktree"));
                    let disabled = [
                        self.worktree_branch_input.value(),
                        self.worktree_path_input.value(),
                        self.worktree_start_input.value(),
                    ]
                    .iter()
                    .any(|text| text.trim().is_empty());
                    (
                        target,
                        rect,
                        node.with_state(AccessState::new().focusable(true).disabled(disabled)),
                    )
                }
                Hit::Panel(PanelHit::CreateBranch) => {
                    let (target, rect, node) =
                        button("git.create-branch".into(), rect, t("git-create-branch"));
                    let disabled = self.branch_input.value().trim().is_empty();
                    (
                        target,
                        rect,
                        node.with_state(AccessState::new().focusable(true).disabled(disabled)),
                    )
                }
                Hit::Panel(PanelHit::Branch(action)) => {
                    let (target, name) = match action {
                        BranchAction::Checkout(name) => (
                            format!("git.branch.checkout:{name}"),
                            t_args("git-check-out-name", &[("name", name.into())]),
                        ),
                        BranchAction::Delete(name) => (
                            format!("git.branch.delete:{name}"),
                            t_args("git-delete-name", &[("name", name.into())]),
                        ),
                        BranchAction::StashApply(index) => (
                            format!("git.stash.apply:{index}"),
                            t_args("git-apply-stash-name", &[("index", (*index).into())]),
                        ),
                    };
                    button(target, rect, name)
                }
                Hit::OpenPath => field(
                    "git.open-path",
                    rect,
                    &self.open_input,
                    &t("git-repository-path"),
                ),
                Hit::ChooseRepository => {
                    button("git.choose-repository".into(), rect, t("git-choose-name"))
                }
                Hit::OpenRepository => {
                    let (target, rect, node) =
                        button("git.open-repository".into(), rect, t("git-open-name"));
                    let disabled = self.open_input.value().trim().is_empty();
                    (
                        target,
                        rect,
                        node.with_state(AccessState::new().focusable(true).disabled(disabled)),
                    )
                }
                Hit::Scrim => continue,
            };
            let (id, rect, node) = entry;
            out.push((id, rect, node, Some(target.hit.clone())));
        }
        out
    }

    /// The node for one entry of the changes column.
    pub(super) fn change_node(
        &self,
        hit: &ChangesHit,
        rect: Rect,
    ) -> Option<(String, Rect, ui::a11y::AccessNode)> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let node = |role: Role, name: String, selected: bool| {
            AccessNode::new(role)
                .with_name(name)
                .with_state(AccessState::new().focusable(true).selected(selected))
                .with_bounds(rect)
        };
        Some(match hit {
            ChangesHit::SelectFile(area, row) => {
                let change = change_list(&self.model, *area).get(*row)?;
                (
                    format!("git.change.open:{area:?}:{}", change.path),
                    rect,
                    node(
                        Role::ListItem,
                        t_args(
                            "git-change-open",
                            &[
                                ("path", change.path.as_str().into()),
                                ("kind", change.kind.label().into()),
                            ],
                        ),
                        self.selected_change == Some((*area, *row)),
                    ),
                )
            }
            ChangesHit::ToggleStage(area, row) => {
                let (verb, key) = if *area == ChangeArea::Staged {
                    ("Unstage", "git-unstage-path")
                } else {
                    ("Stage", "git-stage-path")
                };
                let change = change_list(&self.model, *area).get(*row)?;
                (
                    format!("git.change.{verb}:{area:?}:{}", change.path),
                    rect,
                    node(
                        Role::Button,
                        t_args(key, &[("path", change.path.as_str().into())]),
                        false,
                    ),
                )
            }
            ChangesHit::StageAll(area) => (
                format!("git.changes.stage-all:{area:?}"),
                rect,
                node(Role::Button, t("git-stage-all-name"), false),
            ),
            ChangesHit::UnstageAll(area) => (
                format!("git.changes.unstage-all:{area:?}"),
                rect,
                node(Role::Button, t("git-unstage-all-name"), false),
            ),
            ChangesHit::Commit => (
                "git.changes.commit".into(),
                rect,
                AccessNode::new(Role::Button)
                    .with_name(changes::commit_label(&self.model))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(!self.can_commit()),
                    )
                    .with_bounds(rect),
            ),
            ChangesHit::FocusMessage => return None,
        })
    }
}
