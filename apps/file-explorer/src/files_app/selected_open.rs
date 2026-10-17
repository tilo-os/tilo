//! Admission routing on the existing host lane, distinct from clipboard commands.
use super::{FileExplorerWidget, Ran};
use crate::i18n::t;
use host_fs::RetainedObject;
use std::{path::Path, sync::Arc};
use ui::{
    collaboration::{CollabEvent, CollabRequest, open_selected},
    tilo_actions::InvocationId,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Client {
    pub(super) discovery: super::native_open_with::Discovery,
    pub(super) control: Option<ui::tilo_actions::ControlInvocation>,
    deferred: Option<Deferred>,
    next: u64,
    request: Option<CollabRequest>,
    pending: Option<Pending>,
}
struct Deferred {
    path: std::path::PathBuf,
    request_id: String,
    entry: String,
    app: String,
    invocation: InvocationId,
    access: open_selected::v2::SelectedAccess,
}
enum Completion {
    Admission(open_selected::RootReceipt),
    Observed(open_selected::v2::ObservedReply),
}
enum Command {
    Legacy(open_selected::Request),
    Explicit(open_selected::v2::Request),
}
impl Command {
    fn valid(&self) -> bool {
        match self {
            Self::Legacy(value) => value.valid(),
            Self::Explicit(value) => value.valid(),
        }
    }
    fn request_id(&self) -> &str {
        match self {
            Self::Legacy(value) => &value.request.request_id,
            Self::Explicit(value) => &value.request.request_id,
        }
    }
    fn expected(&self) -> Option<(String, open_selected::v2::SelectedAccess)> {
        match self {
            Self::Legacy(_) => None,
            Self::Explicit(value) => Some((value.request.app_id.clone(), value.request.access)),
        }
    }
    fn request(self, request_id: u64, on_behalf_of: InvocationId) -> CollabRequest {
        match self {
            Self::Legacy(command) => CollabRequest::OpenSelected {
                request_id,
                command,
                on_behalf_of,
            },
            Self::Explicit(command) => CollabRequest::OpenSelectedV2 {
                request_id,
                command,
                on_behalf_of,
            },
        }
    }
}
struct Pending {
    id: u64,
    request_id: String,
    expected: Option<(String, open_selected::v2::SelectedAccess)>,
    proof: Proof,
    _selected: Arc<RetainedObject>,
    _parent: Arc<RetainedObject>,
    deadline: Instant,
    receipt: Option<Result<Completion, String>>,
    verified: bool,
}
use runtime_portal::process_binding::Proof;
impl Client {
    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some() || self.deferred.is_some() || self.request.is_some()
    }

    fn start(
        &mut self,
        command: Command,
        invocation: InvocationId,
        custody: (Arc<RetainedObject>, Arc<RetainedObject>),
        proof: Proof,
    ) -> Result<(), String> {
        if self.is_pending() || !command.valid() {
            return Err(t("files-open-with-busy"));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or(t("files-open-with-launch-unavailable"))?;
        self.pending = Some(Pending {
            id: self.next,
            request_id: command.request_id().to_owned(),
            expected: command.expected(),
            _selected: custody.0,
            _parent: custody.1,
            proof,
            deadline: ui::time::now() + Duration::from_secs(20),
            receipt: None,
            verified: false,
        });
        self.request = Some(command.request(self.next, invocation));
        Ok(())
    }
    pub(super) fn abandon(&mut self) {
        self.deferred.take();
        self.request.take();
        self.pending.take();
    }
    pub(super) fn take_request(&mut self) -> Option<CollabRequest> {
        self.request.take()
    }
    pub(super) fn event(&mut self, event: &CollabEvent) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        if pending.receipt.is_some() {
            return;
        }
        match event {
            CollabEvent::OpenSelectedResult { request_id, result }
                if *request_id == pending.id && pending.expected.is_none() =>
            {
                pending.receipt = Some(result.clone().map(Completion::Admission));
            }
            CollabEvent::OpenSelectedV2Result { request_id, result }
                if *request_id == pending.id && pending.expected.is_some() =>
            {
                pending.receipt = Some(result.clone().map(Completion::Observed));
            }
            _ => {}
        }
    }
    fn poll(&mut self, now: Instant) -> Option<Result<Completion, String>> {
        let pending = self.pending.as_mut()?;
        if now >= pending.deadline {
            self.abandon();
            return Some(Err(t("files-open-with-timeout")));
        }
        if pending.receipt.as_ref().is_some_and(Result::is_err) {
            return self.pending.take()?.receipt;
        }
        if !pending.verified {
            if !pending.proof.finished() {
                return None;
            }
            if pending.proof.result().is_err() {
                self.abandon();
                return Some(Err(t("files-open-with-launch-unavailable")));
            }
            pending.verified = true;
        }
        let result = pending.receipt.take()?;
        let request_id = pending.request_id.clone();
        let expected = pending.expected.clone();
        self.pending.take();
        Some(result.and_then(|completion| {
            validate_completion(&completion, &request_id, expected.as_ref())?;
            Ok(completion)
        }))
    }
}
pub(super) fn components(path: &Path) -> Result<Vec<Vec<u8>>, String> {
    tilo_files::native::retained_home::components(path)
        .ok_or_else(|| t("files-open-with-selection-changed"))
}
impl FileExplorerWidget {
    pub(super) fn begin_selected_open(
        &mut self,
        entry: &str,
        app: &str,
        invocation: Option<InvocationId>,
        always: bool,
    ) -> Result<Ran, String> {
        self.begin_selected_open_access(entry, app, invocation, always, None, None)
    }
    pub(super) fn begin_selected_open_v2(
        &mut self,
        entry: &str,
        app: &str,
        invocation: Option<InvocationId>,
        always: bool,
        access: open_selected::v2::SelectedAccess,
    ) -> Result<Ran, String> {
        self.begin_selected_open_access(entry, app, invocation, always, Some(access), None)
    }
    fn begin_selected_open_access(
        &mut self,
        entry: &str,
        app: &str,
        invocation: Option<InvocationId>,
        always: bool,
        access: Option<open_selected::v2::SelectedAccess>,
        discovered: Option<open_selected::discovery::Reply>,
    ) -> Result<Ran, String> {
        if discovered.is_none() {
            self.refuse_while_answering()?;
        }
        let invocation = invocation.ok_or(t("files-open-with-action-unavailable"))?;
        if access.is_some_and(|access| access.editable())
            && !self.dispatcher.as_ref().is_some_and(|dispatcher| {
                dispatcher.may(&ui::tilo_actions::ActionNeed::of(
                    ui::tilo_actions::ActionKind::Undoable,
                ))
            })
        {
            return Err(t("files-open-with-write-required"));
        }

        if always {
            return Err(t("files-open-with-default-unavailable"));
        }
        let path = self
            .visible_entry_path(entry)
            .ok_or(t("files-open-with-selection-changed"))?;
        if path.parent() != Some(self.current_path.as_path()) {
            return Err(t("files-open-with-selection-changed"));
        }
        let fresh_scope = discovered.is_some();
        if self.native_selected_discovery()
            && let Some(access) = access
        {
            if let Some(scope) = discovered {
                if scope.components != components(&path)?
                    || scope
                        .targets
                        .iter()
                        .find(|target| target.app_id == app)
                        .is_none_or(|target| access.editable() && !target.access.editable())
                {
                    return Err(t("files-open-with-access-unavailable"));
                }
                if access.editable()
                    && !self.dispatcher.as_ref().is_some_and(|dispatcher| {
                        dispatcher.may(&ui::tilo_actions::ActionNeed::of(
                            ui::tilo_actions::ActionKind::Undoable,
                        ))
                    })
                {
                    return Err(t("files-open-with-write-required"));
                }
            } else {
                if self.selected_open.deferred.is_some() {
                    return Err(t("files-open-with-busy"));
                }
                let request_id = self.selected_open.discovery.refresh(
                    &path,
                    open_selected::discovery::InvocationScope {
                        resource: entry.into(),
                        app_id: app.into(),
                        access,
                    },
                    invocation,
                )?;
                self.selected_open.deferred = Some(Deferred {
                    path,
                    request_id,
                    entry: entry.into(),
                    app: app.into(),
                    invocation,
                    access,
                });
                return Ok(Ran::Pending);
            }
        }
        if !fresh_scope && self.open_with_candidates(&path).find(app).is_none() {
            return Err(t("files-open-with-launch-unavailable"));
        }
        self.select_entry(&path)
            .map_err(|_| t("files-open-with-selection-changed"))?;
        let selected = self
            .shown_selection
            .objects
            .get(&path)
            .and_then(|(_, object)| object.clone())
            .ok_or(t("files-open-with-selection-changed"))?;
        if !selected
            .metadata()
            .map_err(|_| t("files-open-with-launch-unavailable"))?
            .is_file()
        {
            return Err(t("files-open-with-selection-changed"));
        }
        let parent = self
            .shown_folder
            .clone()
            .ok_or(t("files-open-with-selection-changed"))?;
        if !parent
            .metadata()
            .map_err(|_| t("files-open-with-launch-unavailable"))?
            .is_dir()
        {
            return Err(t("files-open-with-selection-changed"));
        }
        let proof = Proof::start().map_err(|_| t("files-open-with-launch-unavailable"))?;
        let selected_descriptor = selected
            .descriptor()
            .ok_or(t("files-open-with-selection-changed"))?;
        let parent_descriptor = parent
            .descriptor()
            .ok_or(t("files-open-with-selection-changed"))?;
        let proof_descriptor = proof
            .descriptor()
            .map_err(|_| t("files-open-with-launch-unavailable"))?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let components = components(&path)?;
        let command = match access {
            Some(access) => Command::Explicit(open_selected::v2::Request {
                protocol: open_selected::v2::PROTOCOL.into(),
                resource: entry.into(),
                request: open_selected::v2::RootRequest {
                    schema: 2,
                    request_id,
                    components,
                    app_id: app.into(),
                    access,
                },
                selected_descriptor,
                parent_descriptor,
                proof_descriptor,
            }),
            None => Command::Legacy(open_selected::Request {
                protocol: open_selected::PROTOCOL.into(),
                resource: entry.into(),
                request: open_selected::RootRequest {
                    schema: 1,
                    request_id,
                    components,
                    app_id: app.into(),
                    read_only: true,
                },
                selected_descriptor,
                parent_descriptor,
                proof_descriptor,
            }),
        };
        self.selected_open
            .start(command, invocation, (selected, parent), proof)?;
        self.close_app_chooser();
        Ok(Ran::Pending)
    }
    pub(super) fn poll_selected_open(&mut self) {
        if let Some(deferred) = &self.selected_open.deferred
            && let Some(result) = self
                .selected_open
                .discovery
                .fresh_reply(&deferred.path, &deferred.request_id)
        {
            let Some(deferred) = self.selected_open.deferred.take() else {
                return;
            };
            // The original effect remains pending while current discovery runs.
            // Re-resolve its exact displayed entry before retaining actual FDs;
            // navigation/replacement cannot redirect this original action.
            let result = result.and_then(|scope| {
                self.begin_selected_open_access(
                    &deferred.entry,
                    &deferred.app,
                    Some(deferred.invocation),
                    false,
                    Some(deferred.access),
                    Some(scope),
                )
            });
            if result.is_err()
                && let Some(reply) = self.effect_reply.take()
            {
                reply.answer(Err(ui::tilo_actions::ActionFailure {
                    code: "files.selected_open_unavailable".into(),
                    message: t("files-open-with-launch-unavailable"),
                    details: None,
                    retryable: false,
                }));
            }
        }

        let Some(result) = self.selected_open.poll(ui::time::now()) else {
            return;
        };
        if let Some(reply) = self.effect_reply.take() {
            reply.answer(open_with_outcome(result));
        }
    }
}

/// Compare the result to the actual held request, never to reply labels alone.
fn validate_completion(
    completion: &Completion,
    request_id: &str,
    expected: Option<&(String, open_selected::v2::SelectedAccess)>,
) -> Result<(), String> {
    let valid = match (completion, expected) {
        (Completion::Admission(receipt), None) => {
            receipt.valid() && receipt.request_id == request_id
        }
        (Completion::Observed(observed), Some((app, access))) => {
            observed.valid()
                && observed.access == *access
                && observed.reply.request_id == request_id
                && observed.reply.target.app_id == *app
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(t("files-open-with-selection-changed"))
    }
}

/// Only the host's typed namespace + ordinary launch-readiness completion
/// fulfills OpenWith. Legacy admission remains an honest unavailable result.
fn open_with_outcome(
    result: Result<Completion, String>,
) -> Result<ui::tilo_actions::ActionOutput, ui::tilo_actions::ActionFailure> {
    let (code, message) = match result {
        Ok(Completion::Observed(observed)) => {
            return serde_json::to_value(observed)
                .map(|value| ui::tilo_actions::ActionOutput {
                    value: Some(value),
                    ..Default::default()
                })
                .map_err(|_| ui::tilo_actions::ActionFailure {
                    code: "files.selected_open_result_invalid".into(),
                    message: t("files-open-with-launch-unavailable"),
                    details: None,
                    retryable: false,
                });
        }
        Ok(Completion::Admission(_)) => (
            "files.selected_open_launch_unavailable",
            crate::i18n::t("files-open-with-launch-unavailable"),
        ),
        Err(_) => (
            "files.selected_open_admission_failed",
            t("files-open-with-launch-unavailable"),
        ),
    };
    Err(ui::tilo_actions::ActionFailure {
        code: code.into(),
        message,
        details: None,
        retryable: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_open_with_cannot_succeed_from_admission_only() {
        let receipt = open_selected::RootReceipt {
            schema: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            source_operation: uuid::Uuid::new_v4().to_string(),
        };
        assert!(
            receipt.valid(),
            "a well-formed admission still is not a launch"
        );
        let failure = open_with_outcome(Ok(Completion::Admission(receipt)))
            .expect_err("original OpenWith remains unfulfilled");
        assert_eq!(failure.code, "files.selected_open_launch_unavailable");
        assert!(failure.details.is_none(), "no invented launched-app result");
        let failure =
            open_with_outcome(Err("actual host refusal".into())).expect_err("host refusal");
        assert_eq!(failure.code, "files.selected_open_admission_failed");
        assert_eq!(failure.message, t("files-open-with-launch-unavailable"));
        assert!(!failure.message.contains("actual host refusal"));
    }
    fn observed() -> open_selected::v2::ObservedReply {
        let request_id = uuid::Uuid::new_v4().to_string();
        open_selected::v2::ObservedReply {
            schema: 2,
            access: open_selected::v2::SelectedAccess::EditInParent,
            reply: open_selected::ObservedOpenReply {
                schema: 1,
                request_id: request_id.clone(),
                source_operation: uuid::Uuid::new_v4().to_string(),
                target: open_selected::TargetNamespaceReceipt {
                    schema: 1,
                    request_id,
                    app_id: "so.tilo.text-editor".into(),
                    package_sha256: "a".repeat(64),
                    launcher_pid: 42,
                    launcher_start: 123,
                },
            },
        }
    }
    #[test]
    fn observed_result_must_match_original_request_app_and_access() {
        // Exercises the actual consumer codec/outcome boundary, not native
        // launch qualification: this fixture does not mint kernel authority.
        let reply = observed();
        let request_id = reply.reply.request_id.clone();
        let expected = (reply.reply.target.app_id.clone(), reply.access);
        let completion = Completion::Observed(reply.clone());
        validate_completion(&completion, &request_id, Some(&expected)).unwrap();
        assert!(
            validate_completion(
                &completion,
                &uuid::Uuid::new_v4().to_string(),
                Some(&expected)
            )
            .is_err()
        );
        assert!(
            validate_completion(
                &completion,
                &request_id,
                Some(&("other.app".into(), expected.1))
            )
            .is_err()
        );
        assert!(
            validate_completion(
                &completion,
                &request_id,
                Some(&(
                    expected.0.clone(),
                    open_selected::v2::SelectedAccess::ReadOnly
                ))
            )
            .is_err()
        );
        assert!(validate_completion(&completion, &request_id, None).is_err());
        let output = open_with_outcome(Ok(completion)).expect("matched host observed completion");
        assert_eq!(output.value.unwrap()["access"], "edit-in-parent");
        let legacy = Completion::Admission(open_selected::RootReceipt {
            schema: 1,
            request_id: request_id.clone(),
            source_operation: reply.reply.source_operation,
        });
        assert!(validate_completion(&legacy, &request_id, Some(&expected)).is_err());
        assert!(open_with_outcome(Ok(legacy)).is_err());
    }
}
