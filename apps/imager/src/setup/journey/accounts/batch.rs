//! Explicit bulk consent for supported saved connections; OAuth stays individual.
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use ui::widget::{Checkbox, Column};

#[derive(Default)]
pub(super) struct SavedConnections {
    pub(super) reviewed: bool,
    pub(super) require_review: bool,
    excluded: BTreeSet<String>,
    snapshot: Vec<Selection>,
    queue: Option<VecDeque<Selection>>,
    pub(super) attention: BTreeMap<String, (Selection, &'static str)>,
}
impl SavedConnections {
    pub(super) fn running(&self) -> bool {
        self.queue.is_some()
    }
}
impl Accounts {
    pub(super) fn pending_saved(&self) -> Vec<Selection> {
        self.inventory
            .tilo_accounts
            .iter()
            .cloned()
            .map(Selection::Tilo)
            .chain(
                self.inventory
                    .candidates
                    .iter()
                    .filter(|candidate| candidate.can_import_credentials)
                    .cloned()
                    .map(Selection::Saved),
            )
            .filter(|selected| {
                !self
                    .prepared
                    .iter()
                    .any(|entry| entry.source_id.as_deref() == selected.source_id())
            })
            .collect()
    }
    pub(in crate::setup::journey) fn require_saved_review(&mut self) {
        self.saved.require_review = true;
    }
    pub(in crate::setup::journey) fn continue_label(&self) -> &'static str {
        if self.prepared.is_empty() {
            "installer-accounts-continue-without"
        } else {
            "installer-accounts-continue-included"
        }
    }
    pub(super) fn apply_saved_request(
        &mut self,
        request: &AccountRequest,
    ) -> Result<bool, &'static str> {
        if self.saved.running() && !matches!(request, AccountRequest::Back | AccountRequest::Cancel)
        {
            return Err("installer-accounts-busy");
        }
        match request {
            AccountRequest::ReviewSaved if matches!(self.step, Step::Overview | Step::Done(_)) => {
                self.saved.snapshot = self.pending_saved();
                self.step = Step::ReviewSaved;
            }
            AccountRequest::IncludeSaved(id) | AccountRequest::ExcludeSaved(id)
                if matches!(self.step, Step::ReviewSaved) =>
            {
                if !self
                    .saved
                    .snapshot
                    .iter()
                    .any(|selected| selected.source_id() == Some(id))
                {
                    return Err("installer-accounts-invalid-action");
                }
                if matches!(request, AccountRequest::IncludeSaved(_)) {
                    self.saved.excluded.remove(id);
                } else {
                    self.saved.excluded.insert(id.clone());
                }
            }
            AccountRequest::ImportSaved
                if matches!(
                    self.step,
                    Step::Overview | Step::ReviewSaved | Step::Done(_)
                ) =>
            {
                self.begin_saved_import()?;
            }
            AccountRequest::SkipSaved
                if matches!(
                    self.step,
                    Step::Overview | Step::ReviewSaved | Step::Done(_)
                ) =>
            {
                for selected in self.pending_saved() {
                    if let Some(id) = selected.source_id() {
                        self.saved.excluded.insert(id.to_owned());
                    }
                }
                self.saved.reviewed = true;
                self.step = Step::Overview;
            }
            AccountRequest::Attention(id) if matches!(self.step, Step::Overview) => {
                let (selected, message) = self
                    .saved
                    .attention
                    .get(id)
                    .cloned()
                    .ok_or("installer-accounts-invalid-action")?;
                self.step = Step::Failed {
                    selected,
                    message,
                    retry_cleanup: None,
                };
            }
            AccountRequest::Back | AccountRequest::Cancel if self.saved.running() => {
                self.saved.queue = None;
                if matches!(request, AccountRequest::Cancel)
                    && let Step::Working(work) = &mut self.step
                {
                    work.return_overview = true;
                    return Ok(true);
                }
                // The ordinary working-state Back path retains completed copies.
                return Ok(false);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn begin_saved_import(&mut self) -> Result<(), &'static str> {
        if (self.saved.reviewed || self.saved.require_review)
            && !matches!(self.step, Step::ReviewSaved)
        {
            return Err("installer-accounts-invalid-action");
        }
        let selected = if matches!(self.step, Step::ReviewSaved) {
            self.saved.snapshot.clone()
        } else {
            self.pending_saved()
        };
        let queue: VecDeque<_> = selected
            .into_iter()
            .filter(|selected| {
                selected
                    .source_id()
                    .is_some_and(|id| !self.saved.excluded.contains(id))
            })
            .collect();
        if queue
            .len()
            .checked_add(self.prepared.len())
            .is_none_or(|count| count > 64)
        {
            return Err("installer-accounts-saved-limit");
        }
        self.saved.reviewed = true;
        self.saved.queue = Some(queue);
        self.step = Step::Overview;
        self.poll_saved_queue();
        Ok(())
    }
    pub(super) fn poll_saved_queue(&mut self) -> bool {
        if !matches!(self.step, Step::Overview) || !self.saved.running() {
            return false;
        }
        let selected = self.saved.queue.as_mut().and_then(VecDeque::pop_front);
        if let Some(selected) = selected {
            if let Some(id) = selected.source_id() {
                self.saved.attention.remove(id);
            }
            self.start(selected);
            self.collect_saved_failure();
        } else {
            self.saved.queue = None;
        }
        true
    }
    pub(super) fn collect_saved_failure(&mut self) {
        if !self.saved.running() {
            return;
        }
        let Step::Failed {
            selected,
            message,
            retry_cleanup: None,
        } = &self.step
        else {
            return;
        };
        if let Some(id) = selected.source_id() {
            self.saved
                .attention
                .insert(id.to_owned(), (selected.clone(), *message));
        }
        self.step = Step::Overview;
    }
    pub(super) fn saved_review_view(
        &self,
        mut body: Column,
        dispatch: Rc<dyn Fn(AccountRequest)>,
    ) -> Column {
        for selected in &self.saved.snapshot {
            let Some(id) = selected.source_id() else {
                continue;
            };
            let state = ui::LocalState::new(!self.saved.excluded.contains(id));
            let id = id.to_owned();
            let choose = dispatch.clone();
            body = body.push(Box::new(
                Checkbox::from_state(&state)
                    .with_id(&format!("installer.account.include.{id}"))
                    .with_label(selected.label())
                    .on_change(move |include| {
                        choose(if include {
                            AccountRequest::IncludeSaved(id.clone())
                        } else {
                            AccountRequest::ExcludeSaved(id.clone())
                        })
                    }),
            ));
        }
        body
    }
    pub(super) fn saved_overview_view(
        &self,
        mut body: Column,
        dispatch: Rc<dyn Fn(AccountRequest)>,
    ) -> Column {
        let pending = self.pending_saved();
        if !pending.is_empty() {
            body = body.push(Box::new(Text::new(&t_args(
                "installer-accounts-saved-summary",
                &[("count", pending.len().into())],
            ))));
            for selected in &pending {
                let Some(id) = selected.source_id() else {
                    continue;
                };
                body = body.push(Box::new(
                    ListRow::new(format!("installer.account.detected.{id}"), selected.label())
                        .secondary(t(if self.saved.excluded.contains(id) {
                            "installer-accounts-left-out"
                        } else {
                            "installer-accounts-saved"
                        }))
                        .two_line_secondary()
                        .icon(Icon::Cloud),
                ));
            }
            body = body.push(Box::new(
                action_button(
                    "installer.account.review-saved",
                    "installer-accounts-review-saved",
                    AccountRequest::ReviewSaved,
                    dispatch.clone(),
                )
                .role(ButtonRole::Plain),
            ));
            if !self.saved.reviewed {
                body = body.push(Box::new(
                    action_button(
                        "installer.account.skip-saved",
                        "installer-accounts-skip-saved",
                        AccountRequest::SkipSaved,
                        dispatch.clone(),
                    )
                    .role(ButtonRole::Plain),
                ));
            }
        }
        for (id, (selected, _)) in &self.saved.attention {
            let choose = dispatch.clone();
            let id = id.clone();
            body = body.push(Box::new(
                ListRow::new(
                    format!("installer.account.attention.{id}"),
                    selected.label(),
                )
                .secondary(t("installer-accounts-saved-attention"))
                .icon(Icon::Cloud)
                .on_activate(move || choose(AccountRequest::Attention(id.clone()))),
            ));
        }
        body
    }
}
