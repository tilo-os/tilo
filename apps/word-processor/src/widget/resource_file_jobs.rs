//! Original caller custody belongs to each accepted file operation.
use super::{AutoCommit, WordProcessorWidget, WpFormat};
use ui::invocation_scope::ResourceInvocationScope;

/// File effects name native paths; a host without them (a browser page) has no
/// file or asset transport.
fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

impl WordProcessorWidget {
    pub(super) fn start_file_operation(
        &mut self,
        ui::widget::OperationRequest {
            invocation,
            kind,
            type_id,
            payload,
        }: ui::widget::OperationRequest<'_>,
        doc: &AutoCommit,
        scope: Option<&ResourceInvocationScope>,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use crate::actions::WordProcessorOperation;
        if kind == tilo_actions::OperationKind::Effect && !native_paths() {
            return Err("file operation unsupported: no browser file/asset transport".into());
        }

        if kind == tilo_actions::OperationKind::Document {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if self.file_choice.is_some()
            && !matches!(operation, WordProcessorOperation::ResolveFileChoice { .. })
        {
            return Err("Resolve the selected file choice before another operation".into());
        }
        if self.scoped_file_results.len() + self.file_job_scopes.len() >= 32 {
            return Err("unconsumed file operation results reached the bound".into());
        }
        if let Some(scope) = scope {
            scope.check()?;
        }
        let (destination, force_picker) = match operation {
            WordProcessorOperation::ResolveFileChoice {
                generation,
                use_selected,
            } => {
                if self.pending_file_choice.is_some()
                    || self
                        .file_choice
                        .as_ref()
                        .is_none_or(|offer| offer.generation != generation)
                {
                    return Err("selected file choice is no longer current".into());
                }
                self.retain_file_job(invocation, scope)?;
                self.pending_file_choice = Some((invocation, generation, use_selected));
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            WordProcessorOperation::Open { path } => {
                if scope.is_some() {
                    return Err("scoped Open requires an admitted destination record mount".into());
                }
                if self.portal_rx.is_some()
                    || self.pending_resource_open.is_some()
                    || self.pending_replacement.is_some()
                {
                    return Err("another file operation is already running".into());
                }
                if let Some(path) = path {
                    self.pending_resource_open = Some((invocation, std::path::PathBuf::from(path)));
                } else {
                    self.request_open_document();
                    self.resource_file_invocation = Some(invocation);
                }
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            WordProcessorOperation::Save { path } => (path, false),
            WordProcessorOperation::SaveAs { path } => (path, true),
            other => {
                let encoded = serde_json::to_vec(&other).map_err(|error| error.to_string())?;
                return Ok(self
                    .apply_operation(kind, type_id, &encoded, doc)?
                    .then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    }));
            }
        };

        if self.portal_rx.is_some()
            || self.pending_resource_open.is_some()
            || self.pending_replacement.is_some()
        {
            return Err("another file operation is already running".into());
        }
        if let Some(destination) = destination {
            let destination = std::path::PathBuf::from(destination);
            let format = WpFormat::from_path(&destination);
            self.try_write_to_path_for_job(doc, &destination, format, scope)?;
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        } else if self.path.is_some() && !force_picker {
            let destination = self.path.clone().ok_or("document has no writable path")?;
            let format = self.format.ok_or("document has no writable format")?;
            self.try_write_to_path_for_job(doc, &destination, format, scope)?;
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        } else {
            self.retain_file_job(invocation, scope)?;
            if let Err(error) = self.request_save_as_for_job(scope) {
                self.file_job_scopes.remove(&invocation);
                return Err(error);
            }
            self.resource_file_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        }
    }

    fn retain_file_job(
        &mut self,
        invocation: tilo_actions::InvocationId,
        scope: Option<&ResourceInvocationScope>,
    ) -> Result<(), String> {
        if let Some(scope) = scope {
            scope.check()?;
            if scope.invocation() != invocation
                || self.file_job_scopes.contains_key(&invocation)
                || self.scoped_file_results.contains_key(&invocation)
            {
                return Err("file job has mismatched or repeated original invocation".into());
            }
            if self.file_job_scopes.len() >= 32 {
                return Err("file job custody limit reached".into());
            }
            self.file_job_scopes.insert(invocation, scope.clone());
        }
        Ok(())
    }

    pub(super) fn finish_file_operation(&mut self, update: tilo_actions::InvocationUpdate) {
        let invocation = update.invocation();
        if self.file_job_scopes.remove(&invocation).is_some() {
            self.scoped_file_results.insert(invocation, update);
        } else {
            self.resource_file_result = Some(update);
        }
    }

    pub(super) fn expire_file_jobs(&mut self) {
        let expired: Vec<_> = self
            .file_job_scopes
            .iter()
            .filter_map(|(id, scope)| scope.check().err().map(|error| (*id, error)))
            .collect();
        for (invocation, message) in expired {
            if self
                .pending_resource_open
                .as_ref()
                .is_some_and(|(id, _)| *id == invocation)
            {
                self.pending_resource_open = None;
            }
            if self
                .pending_file_choice
                .as_ref()
                .is_some_and(|(id, _, _)| *id == invocation)
            {
                self.pending_file_choice = None;
            }
            if self.resource_file_invocation == Some(invocation) {
                self.portal_rx = None;
                self.portal_purpose = None;
                self.pending_replacement = None;
                self.replace_after_save = false;
                self.unsaved_changes.close();
                self.resource_file_invocation = None;
            }
            self.finish_file_operation(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "word_processor.original_caller_unavailable".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            });
        }
    }

    pub(super) fn resolve_file_job(
        &mut self,
        state: &mut AutoCommit,
        invocation: tilo_actions::InvocationId,
        generation: u64,
        use_selected: bool,
    ) -> Result<(), String> {
        let scope = self.file_job_scopes.get(&invocation).cloned();
        match scope {
            Some(scope) => scope.with_document_mutation(state, |state| {
                self.resolve_file_choice_for_job(state, generation, use_selected, Some(&scope))
            }),
            None => self.resolve_file_choice(state, generation, use_selected),
        }
    }
}

impl Drop for WordProcessorWidget {
    fn drop(&mut self) {
        for scope in self.file_job_scopes.values() {
            scope.cancel();
        }
    }
}
