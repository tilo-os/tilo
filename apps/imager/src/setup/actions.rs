//! Process-local semantic operations for the guided setup workflow.
//!
//! Setup choices describe local navigation and a reviewable plan. They are not
//! replayable document actions and never carry folder paths or installation
//! authority into the shared Automerge document.

use tilo_actions::{
    ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    ResourcePresentation, ResultSpec,
};

use super::controller::SetupPage;
use super::model::{Experience, FolderMode};

pub const SETUP_RESOURCE_ID: &str = "imager.setup";
pub const SETUP_OPERATION_TYPE: &str = "imager.setup.operation";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SetupOperation {
    ChooseExperience {
        experience: String,
    },
    SetCommonMode {
        mode: String,
    },
    SetFolderMode {
        folder: String,
        mode: String,
    },
    ClearFolderMode {
        folder: String,
    },
    RemoveFolder {
        folder: String,
    },
    SelectSuggestion {
        suggestion: String,
    },
    ChooseFolder,
    ToggleHelp,
    Next,
    Back,
    OpenImports,
    StartInternalDownload,
    PauseInternalDownload,
    ResumeInternalDownload,
    CancelInternalDownload,
    DiscardInternalDownload,
    ReleaseInternalDownload,
    /// Tilo size in whole GiB, within the reviewed bounds.
    SetInternalTargetSize {
        gib: i64,
    },
    /// Install (or continue) exactly the plan with this digest.
    InstallInternal {
        reviewed: String,
    },
    RestartInternal,
    RestartInternalLater,
    /// Arm the unfinished operation with this ID again.
    RetryInternal {
        operation_id: String,
    },
    /// Remove the unfinished operation with this ID.
    RemoveInternal {
        operation_id: String,
    },
    InternalFirmwareSettings,
    InternalDisableFastStartup,
    RecheckInternal,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FolderProjection {
    pub identity: String,
    pub index: usize,
    pub mode: &'static str,
    pub overridden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SuggestionProjection {
    pub identity: String,
    pub name: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SelectionProjection {
    pub experience: Option<&'static str>,
    pub common_mode: &'static str,
    pub folder_count: usize,
    pub active_folder_count: usize,
    pub folders: Vec<FolderProjection>,
    pub suggestions: Vec<SuggestionProjection>,
    pub help_open: bool,
    pub imports_open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReviewProjection {
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AvailabilityProjection {
    pub desktop: bool,
    pub full_os: bool,
    pub connected: bool,
    pub next: bool,
    pub back: bool,
    pub folder_policies: bool,
    pub folder_edits: bool,
    pub help: bool,
    pub open_imports: bool,
    pub choose_folder: bool,
}

/// Which Full Tilo installation commands may run now.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent availability flag per published action"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalAvailability {
    pub start_download: bool,
    pub pause_download: bool,
    pub resume_download: bool,
    pub cancel_download: bool,
    pub discard_download: bool,
    pub release_download: bool,
    pub set_target_size: bool,
    pub install: bool,
    pub restart: bool,
    pub restart_later: bool,
    pub retry: bool,
    pub remove: bool,
    pub firmware_settings: bool,
    pub disable_fast_startup: bool,
    pub recheck: bool,
}

/// Path-free state of the Full Tilo installation on this computer.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalProjection {
    /// `gated`, `checking`, `no-release`, `unsupported` or `ready`.
    pub readiness: &'static str,
    pub on_review: bool,
    /// Download worker phase (`none` before any download).
    pub download: &'static str,
    pub target_gib: Option<u64>,
    pub min_gib: Option<u64>,
    pub max_gib: Option<u64>,
    /// Digest `setup.internal.install` must name; changes with the plan.
    pub reviewed: Option<String>,
    pub shrink_bytes: Option<u64>,
    pub staging_bytes: Option<u64>,
    /// Stable blocker IDs from the host probe.
    pub blockers: Vec<&'static str>,
    /// `idle`, `running`, `succeeded` or `failed`.
    pub run: &'static str,
    /// Helper action of the latest run (`stage`, `arm`, `remove`).
    pub run_action: Option<&'static str>,
    pub failure: Option<String>,
    /// Unfinished staging operation the resume card offers.
    pub pending_operation: Option<String>,
    pub availability: InternalAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SetupProjection {
    pub page: &'static str,
    pub selection: SelectionProjection,
    pub review: ReviewProjection,
    pub availability: AvailabilityProjection,
    pub internal: InternalProjection,
}

impl SetupProjection {
    #[must_use]
    pub fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

#[must_use]
pub const fn page_key(page: SetupPage) -> &'static str {
    match page {
        SetupPage::Experience => "experience",
        SetupPage::Folders => "folders",
        SetupPage::Review => "review",
    }
}

#[must_use]
pub const fn experience_key(experience: Experience) -> &'static str {
    match experience {
        Experience::Desktop => "desktop",
        Experience::FullOs => "full_os",
    }
}

#[must_use]
pub const fn mode_key(mode: FolderMode) -> &'static str {
    match mode {
        FolderMode::Copy => "copy",
        FolderMode::Connected => "connected",
        FolderMode::Later => "later",
    }
}

pub fn parse_experience(value: &str) -> Result<Experience, String> {
    match value {
        "desktop" => Ok(Experience::Desktop),
        "full_os" => Ok(Experience::FullOs),
        _ => Err(format!("unknown setup experience `{value}`")),
    }
}

pub fn parse_mode(value: &str) -> Result<FolderMode, String> {
    match value {
        "copy" => Ok(FolderMode::Copy),
        "connected" => Ok(FolderMode::Connected),
        "later" => Ok(FolderMode::Later),
        _ => Err(format!("unknown folder mode `{value}`")),
    }
}

#[must_use]
pub fn encode(operation: &SetupOperation) -> EncodedAction {
    EncodedAction::encode_operation(SETUP_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: SETUP_OPERATION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

/// Decode an app-owned setup operation without admitting a document mutation.
///
/// # Errors
///
/// Returns the JSON error when this is the setup operation type but the
/// payload is malformed.
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<SetupOperation>, String> {
    if type_id != SETUP_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| format!("invalid setup operation: {error}"))
}

fn result_spec() -> ResultSpec {
    ResultSpec::Json {
        schema: serde_json::json!({ "type": "object" }),
    }
}

fn immediate(id: &str, label: &str, operation: SetupOperation) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        &operation,
    )
    .unwrap_or_else(|_| {
        ActionSpec::typed_operation(
            id,
            label,
            "",
            OperationKind::View,
            SETUP_OPERATION_TYPE,
            Vec::new(),
            move |_| encode(&operation),
        )
    })
    .returns(result_spec())
}

fn with_availability(action: ActionSpec, available: bool, code: &str, reason: &str) -> ActionSpec {
    if available {
        action
    } else {
        action.unavailable(code, reason)
    }
}

fn folder_resource(
    folder: &FolderProjection,
    mode_choices: &[&'static str],
    edits_available: bool,
) -> Resource {
    let identity = folder.identity.clone();
    let set_identity = identity.clone();
    let set_mode = ActionSpec::typed_operation(
        "set-mode",
        "Set folder mode",
        "copy",
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![
            ActionParam::required_owned("mode", ParamType::Text, "Folder behavior")
                .with_choices(mode_choices.iter().copied()),
        ],
        move |args| {
            encode(&SetupOperation::SetFolderMode {
                folder: set_identity.clone(),
                mode: args.text("mode").unwrap_or("").to_string(),
            })
        },
    )
    .returns(result_spec());
    let set_mode = with_availability(
        set_mode,
        edits_available,
        "folder_choices_not_open",
        "Folder choices are not open on the current setup page",
    );
    let clear_mode = with_availability(
        immediate(
            "use-common-mode",
            "Use the common folder mode",
            SetupOperation::ClearFolderMode {
                folder: identity.clone(),
            },
        ),
        edits_available && folder.overridden,
        if edits_available {
            "folder_mode_inherited"
        } else {
            "folder_choices_not_open"
        },
        if edits_available {
            "This folder already follows the common choice"
        } else {
            "Folder choices are not open on the current setup page"
        },
    );
    let remove = with_availability(
        immediate(
            "remove",
            "Remove folder from setup",
            SetupOperation::RemoveFolder {
                folder: identity.clone(),
            },
        ),
        edits_available,
        "folder_choices_not_open",
        "Folder choices are not open on the current setup page",
    );

    Resource::new(
        format!("{SETUP_RESOURCE_ID}.folder.{identity}"),
        "imager.setup.folder",
        format!("Selected folder {}", folder.index + 1),
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "{} · {}",
            folder.mode,
            if folder.overridden {
                "override"
            } else {
                "common"
            }
        )),
        badge: Some(format!("Folder {}", folder.index + 1)),
        preview: None,
    })
    .with_action(set_mode)
    .with_action(clear_mode)
    .with_action(remove)
}

fn suggestion_resource(suggestion: &SuggestionProjection, choices_open: bool) -> Resource {
    let available = choices_open && suggestion.available;
    let select = with_availability(
        immediate(
            "select",
            "Add suggested folder",
            SetupOperation::SelectSuggestion {
                suggestion: suggestion.identity.clone(),
            },
        ),
        available,
        if choices_open {
            "suggestion_unavailable"
        } else {
            "folder_choices_not_open"
        },
        if choices_open {
            suggestion
                .unavailable_reason
                .as_deref()
                .unwrap_or("This suggested folder is unavailable")
        } else {
            "Folder choices are not open on the current setup page"
        },
    );
    Resource::new(
        format!("{SETUP_RESOURCE_ID}.suggestion.{}", suggestion.identity),
        "imager.setup.suggestion",
        format!("Suggested {} folder", suggestion.name),
    )
    .with_presentation(ResourcePresentation {
        detail: Some(
            if suggestion.available {
                "Available"
            } else {
                "Unavailable"
            }
            .into(),
        ),
        badge: Some("Suggestion".into()),
        preview: None,
    })
    .with_action(select)
}

fn internal_action(
    id: &str,
    label_key: &str,
    operation: SetupOperation,
    available: bool,
    code: &str,
    reason_key: &str,
) -> ActionSpec {
    with_availability(
        immediate(id, &super::strings::t(label_key), operation),
        available,
        code,
        &super::strings::t(reason_key),
    )
}

/// Full Tilo installation resource: one action per visible command.
#[allow(
    clippy::too_many_lines,
    reason = "one declarative list keeps every published command beside its gate"
)]
fn internal_resource(state: &InternalProjection) -> Resource {
    let a = state.availability;
    let (review_code, review_reason) = if state.on_review {
        (
            "internal_unavailable",
            "imager-setup-internal-error-command-unavailable",
        )
    } else {
        (
            "full_os_review_only",
            "imager-setup-internal-error-not-review",
        )
    };
    let download = |id: &str, label: &str, operation: SetupOperation, available: bool| {
        internal_action(id, label, operation, available, review_code, review_reason)
    };
    let set_size = ActionSpec::typed_operation(
        "setup.internal.set-target-size",
        super::strings::t("imager-setup-action-internal-set-target-size"),
        "64",
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![ActionParam::required_owned(
            "gib",
            ParamType::Integer,
            super::strings::t("imager-setup-param-internal-target-gib"),
        )],
        |args| {
            encode(&SetupOperation::SetInternalTargetSize {
                gib: args.integer("gib").unwrap_or(-1),
            })
        },
    )
    .returns(result_spec());
    let set_size = with_availability(
        set_size,
        a.set_target_size,
        review_code,
        &super::strings::t(review_reason),
    );
    let operation = state.pending_operation.clone().unwrap_or_default();
    Resource::new(
        "imager.setup.internal",
        "imager.setup.internal",
        super::strings::t("imager-setup-resource-internal"),
    )
    .with_presentation(ResourcePresentation {
        detail: Some(super::strings::t_args(
            "imager-setup-detail-internal",
            &[
                ("readiness", state.readiness.into()),
                ("download", state.download.into()),
                ("run", state.run.into()),
                (
                    "blockers",
                    if state.blockers.is_empty() {
                        "none".to_string()
                    } else {
                        state.blockers.join(", ")
                    }
                    .into(),
                ),
            ],
        )),
        badge: Some(super::strings::t("imager-setup-badge-internal")),
        preview: None,
    })
    .with_action(download(
        "setup.internal.download",
        "imager-setup-action-start-download",
        SetupOperation::StartInternalDownload,
        a.start_download,
    ))
    .with_action(download(
        "setup.internal.pause-download",
        "imager-setup-action-pause-download",
        SetupOperation::PauseInternalDownload,
        a.pause_download,
    ))
    .with_action(download(
        "setup.internal.resume-download",
        "imager-setup-action-resume-download",
        SetupOperation::ResumeInternalDownload,
        a.resume_download,
    ))
    .with_action(download(
        "setup.internal.cancel-download",
        "imager-setup-action-cancel-download",
        SetupOperation::CancelInternalDownload,
        a.cancel_download,
    ))
    .with_action(download(
        "setup.internal.discard-download",
        "imager-setup-action-discard-download",
        SetupOperation::DiscardInternalDownload,
        a.discard_download,
    ))
    .with_action(download(
        "setup.internal.release-download",
        "imager-setup-action-release-download",
        SetupOperation::ReleaseInternalDownload,
        a.release_download,
    ))
    .with_action(set_size)
    .with_action(internal_action(
        "setup.internal.install",
        "imager-setup-action-internal-install",
        SetupOperation::InstallInternal {
            reviewed: state.reviewed.clone().unwrap_or_default(),
        },
        a.install,
        review_code,
        review_reason,
    ))
    .with_action(internal_action(
        "setup.internal.restart",
        "imager-setup-action-internal-restart",
        SetupOperation::RestartInternal,
        a.restart,
        "internal_not_staged",
        "imager-setup-internal-error-no-restart",
    ))
    .with_action(internal_action(
        "setup.internal.restart-later",
        "imager-setup-action-internal-restart-later",
        SetupOperation::RestartInternalLater,
        a.restart_later,
        "internal_not_staged",
        "imager-setup-internal-error-no-restart",
    ))
    .with_action(internal_action(
        "setup.internal.retry",
        "imager-setup-action-internal-retry",
        SetupOperation::RetryInternal {
            operation_id: operation.clone(),
        },
        a.retry,
        "internal_no_pending",
        "imager-setup-internal-error-no-pending",
    ))
    .with_action(internal_action(
        "setup.internal.remove",
        "imager-setup-action-internal-remove",
        SetupOperation::RemoveInternal {
            operation_id: operation,
        },
        a.remove,
        "internal_no_pending",
        "imager-setup-internal-error-no-pending",
    ))
    .with_action(internal_action(
        "setup.internal.firmware-settings",
        "imager-setup-action-internal-firmware-settings",
        SetupOperation::InternalFirmwareSettings,
        a.firmware_settings,
        "remedy_unavailable",
        "imager-setup-internal-error-remedy-unavailable",
    ))
    .with_action(internal_action(
        "setup.internal.disable-fast-startup",
        "imager-setup-action-internal-disable-fast-startup",
        SetupOperation::InternalDisableFastStartup,
        a.disable_fast_startup,
        "remedy_unavailable",
        "imager-setup-internal-error-remedy-unavailable",
    ))
    .with_action(internal_action(
        "setup.internal.recheck",
        "imager-setup-action-internal-recheck",
        SetupOperation::RecheckInternal,
        a.recheck,
        "internal_unavailable",
        "imager-setup-internal-error-unavailable",
    ))
}

/// Build the path-free, live resource projection for the setup root.
#[must_use]
pub fn resource(state: &SetupProjection) -> Resource {
    let experience_choices = [
        state.availability.desktop.then_some("desktop"),
        state.availability.full_os.then_some("full_os"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let choose_experience = ActionSpec::typed_operation(
        "choose-experience",
        "Choose setup experience",
        "desktop",
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "experience",
                ParamType::Text,
                "Available setup experience",
            )
            .with_choices(experience_choices),
        ],
        |args| {
            encode(&SetupOperation::ChooseExperience {
                experience: args.text("experience").unwrap_or("").to_string(),
            })
        },
    )
    .returns(result_spec());
    let choose_experience = with_availability(
        choose_experience,
        state.availability.desktop || state.availability.full_os,
        "no_setup_experience",
        "No setup experience is available on this computer",
    );
    let choose_folder = with_availability(
        immediate(
            "choose-folder",
            "Choose a folder",
            SetupOperation::ChooseFolder,
        ),
        state.availability.choose_folder,
        "folder_picker_unavailable",
        "The folder picker is not available on this setup page",
    );

    let mode_choices = [
        Some("copy"),
        state.availability.connected.then_some("connected"),
        Some("later"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let common_mode = ActionSpec::typed_operation(
        "set-common-mode",
        "Set common folder mode",
        "copy",
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "mode",
                ParamType::Text,
                "Folder behavior applied to folders without an override",
            )
            .with_choices(mode_choices.clone()),
        ],
        |args| {
            encode(&SetupOperation::SetCommonMode {
                mode: args.text("mode").unwrap_or("").to_string(),
            })
        },
    )
    .returns(result_spec());
    let common_mode = with_availability(
        common_mode,
        state.availability.folder_policies,
        "folder_choices_not_open",
        "Folder choices are not open on the current setup page",
    );

    let help = with_availability(
        immediate(
            "toggle-help",
            "Show or hide folder help",
            SetupOperation::ToggleHelp,
        ),
        state.availability.help,
        "help_not_on_page",
        "Folder help is available on the folder page",
    );
    let next = with_availability(
        immediate("next", "Continue setup", SetupOperation::Next),
        state.availability.next,
        "setup_cannot_continue",
        "Complete the current setup choice before continuing",
    );
    let back = with_availability(
        immediate("back", "Go back in setup", SetupOperation::Back),
        state.availability.back,
        "setup_at_start",
        "Setup is already on its first page",
    );

    let folder_resources = state
        .selection
        .folders
        .iter()
        .map(|folder| folder_resource(folder, &mode_choices, state.availability.folder_edits))
        .collect::<Vec<_>>();
    let selection = Resource::new(
        "imager.setup.selection",
        "imager.setup.selection",
        "Current setup selection",
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "experience {} · common {} · {} folders · {} active · help {} · imports {}",
            state
                .selection
                .experience
                .unwrap_or("no experience selected"),
            state.selection.common_mode,
            state.selection.folder_count,
            state.selection.active_folder_count,
            open_closed(state.selection.help_open),
            open_closed(state.selection.imports_open),
        )),
        badge: Some("Selection".into()),
        preview: None,
    })
    .with_children(folder_resources);
    let suggestions =
        Resource::new(
            "imager.setup.suggestions",
            "imager.setup.suggestions",
            "Suggested folders",
        )
        .with_children(
            state.selection.suggestions.iter().map(|suggestion| {
                suggestion_resource(suggestion, state.availability.choose_folder)
            }),
        );
    let review = Resource::new(
        "imager.setup.review",
        "imager.setup.review",
        "Setup review state",
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "ready {} · current page {}",
            yes_no(state.review.ready),
            state.page
        )),
        badge: Some("Review".into()),
        preview: None,
    });
    let availability = Resource::new(
        "imager.setup.availability",
        "imager.setup.availability",
        "Current setup availability",
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "Desktop {} · Full OS {} · Connected {} · Next {} · Back {} · Folder policies {} · Folder edits {} · Help {} · Imports {}",
            yes_no(state.availability.desktop),
            yes_no(state.availability.full_os),
            yes_no(state.availability.connected),
            yes_no(state.availability.next),
            yes_no(state.availability.back),
            yes_no(state.availability.folder_policies),
            yes_no(state.availability.folder_edits),
            yes_no(state.availability.help),
            yes_no(state.availability.open_imports),
        )),
        badge: Some("Availability".into()),
        preview: None,
    });

    Resource::new(SETUP_RESOURCE_ID, "imager.setup", "Guided Tilo setup")
        .with_presentation(ResourcePresentation {
            detail: Some(format!(
                "{} · {} · {} · {} folder(s) · review {} · Desktop {} · Full OS {} · Connected {} · Next {} · Back {}",
                state.page,
                state
                    .selection
                    .experience
                    .unwrap_or("no experience selected"),
                state.selection.common_mode,
                state.selection.folder_count,
                yes_no(state.review.ready),
                yes_no(state.availability.desktop),
                yes_no(state.availability.full_os),
                yes_no(state.availability.connected),
                yes_no(state.availability.next),
                yes_no(state.availability.back),
            )),
            badge: Some(
                match state.page {
                    "experience" => "Experience",
                    "folders" => "Folders",
                    "review" => "Review",
                    _ => "Setup",
                }
                .into(),
            ),
            preview: None,
        })
        .with_action(choose_experience)
        .with_action(common_mode)
        .with_action(choose_folder)
        .with_action(help)
        .with_action(next)
        .with_action(back)
        .with_action(with_availability(
            immediate("open-imports", "Open folder imports", SetupOperation::OpenImports),
            state.availability.open_imports,
            "imports_not_on_page",
            "Folder imports are available on the experience and review pages",
        ))
        .with_children([
            selection,
            suggestions,
            review,
            availability,
            internal_resource(&state.internal),
        ])
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

const fn open_closed(value: bool) -> &'static str {
    if value { "open" } else { "closed" }
}

/// Guided setup as a resource. The live projection is view state, not a shared
/// document; [`publish`](ui::tilo_resource::publish) still builds the palette
/// from it.
pub struct SetupGuide;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SetupDocumentAction {}

impl ui::tilo_actions::Action for SetupDocumentAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ui::tilo_actions::ActionError> {
        match self {}
    }
}

impl ui::tilo_resource::Resource for SetupGuide {
    const KIND: &'static str = "imager.setup";
    const LABEL: &'static str = "Guided Tilo setup";
    const ACTION_TYPE: &'static str = "imager.setup.document";

    type State = SetupProjection;
    type Action = SetupDocumentAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        registry.register::<SetupDocumentAction>(Self::ACTION_TYPE);
    }

    fn read(
        _doc: &ui::tilo_resource::Doc,
    ) -> Result<SetupProjection, ui::tilo_resource::ModelError> {
        Ok(SetupProjection {
            page: "experience",
            selection: SelectionProjection {
                experience: None,
                common_mode: "copy",
                folder_count: 0,
                active_folder_count: 0,
                folders: Vec::new(),
                suggestions: Vec::new(),
                help_open: false,
                imports_open: false,
            },
            review: ReviewProjection { ready: false },
            availability: AvailabilityProjection {
                desktop: false,
                full_os: false,
                connected: false,
                next: false,
                back: false,
                folder_policies: false,
                folder_edits: false,
                help: false,
                open_imports: false,
                choose_folder: false,
            },
            internal: InternalProjection {
                readiness: "gated",
                on_review: false,
                download: "none",
                target_gib: None,
                min_gib: None,
                max_gib: None,
                reviewed: None,
                shrink_bytes: None,
                staging_bytes: None,
                blockers: Vec::new(),
                run: "idle",
                run_action: None,
                failure: None,
                pending_operation: None,
                availability: InternalAvailability::default(),
            },
        })
    }

    fn bound_actions(state: &SetupProjection) -> Vec<ActionSpec> {
        resource(state).actions
    }

    fn children(state: &SetupProjection) -> Vec<Resource> {
        resource(state).children
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection(internal: InternalProjection) -> SetupProjection {
        SetupProjection {
            page: "review",
            selection: SelectionProjection {
                experience: Some("full_os"),
                common_mode: "copy",
                folder_count: 0,
                active_folder_count: 0,
                folders: Vec::new(),
                suggestions: Vec::new(),
                help_open: false,
                imports_open: false,
            },
            review: ReviewProjection { ready: true },
            availability: AvailabilityProjection {
                desktop: false,
                full_os: false,
                connected: false,
                next: false,
                back: true,
                folder_policies: false,
                folder_edits: false,
                help: false,
                open_imports: true,
                choose_folder: false,
            },
            internal,
        }
    }

    fn internal_actions(state: &SetupProjection) -> Vec<ActionSpec> {
        resource(state)
            .children
            .into_iter()
            .find(|child| child.kind == "imager.setup.internal")
            .expect("internal resource")
            .actions
    }

    fn code(action: &ActionSpec) -> Option<&str> {
        match &action.contract.availability {
            tilo_actions::Availability::Available => None,
            tilo_actions::Availability::Unavailable { code, .. } => Some(code),
        }
    }

    /// Internal install resource commands, availability and payload.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_resource_publishes_gated_commands_with_reviewed_digest() {
        // Phase `internal_resource_publishes_every_install_command`.
        // Catches an install command reachable only by clicking (spec §8).
        {
            let actions = internal_actions(&projection(InternalProjection::default()));
            for id in [
                "setup.internal.set-target-size",
                "setup.internal.install",
                "setup.internal.restart",
                "setup.internal.retry",
                "setup.internal.remove",
                "setup.internal.firmware-settings",
                "setup.internal.disable-fast-startup",
                "setup.internal.restart-later",
                "setup.internal.recheck",
                "setup.internal.download",
                "setup.internal.pause-download",
                "setup.internal.resume-download",
                "setup.internal.cancel-download",
                "setup.internal.discard-download",
                "setup.internal.release-download",
            ] {
                let action = actions
                    .iter()
                    .find(|action| action.id == id)
                    .unwrap_or_else(|| panic!("missing {id}"));
                assert!(!action.contract.availability.is_available(), "{id}");
            }
            let install = actions
                .iter()
                .find(|action| action.id == "setup.internal.install")
                .unwrap();
            assert_eq!(code(install), Some("full_os_review_only"));
        }

        // Phase `internal_availability_follows_each_flag`.
        // Catches availability flags wired to the wrong action.
        {
            let actions = internal_actions(&projection(InternalProjection {
                on_review: true,
                availability: InternalAvailability {
                    install: true,
                    firmware_settings: true,
                    ..InternalAvailability::default()
                },
                ..InternalProjection::default()
            }));
            for action in &actions {
                let expected = matches!(
                    action.id.as_str(),
                    "setup.internal.install" | "setup.internal.firmware-settings"
                );
                assert_eq!(
                    action.contract.availability.is_available(),
                    expected,
                    "{}",
                    action.id
                );
            }
            let restart = actions
                .iter()
                .find(|action| action.id == "setup.internal.restart")
                .unwrap();
            assert_eq!(code(restart), Some("internal_not_staged"));
        }

        // Phase `internal_actions_carry_the_reviewed_digest_and_operation`.
        // Catches an install or removal that does not name what the invoker saw,
        // which is what lets the view refuse a stale invocation.
        {
            let state = projection(InternalProjection {
                on_review: true,
                reviewed: Some("a".repeat(64)),
                pending_operation: Some("op-7".into()),
                ..InternalProjection::default()
            });
            let actions = internal_actions(&state);
            let decode_action = |id: &str| {
                let encoded = actions
                    .iter()
                    .find(|action| action.id == id)
                    .unwrap()
                    .build_json(&serde_json::Map::new())
                    .unwrap();
                decode(&encoded.type_id, &encoded.payload).unwrap().unwrap()
            };
            assert_eq!(
                decode_action("setup.internal.install"),
                SetupOperation::InstallInternal {
                    reviewed: "a".repeat(64)
                }
            );
            assert_eq!(
                decode_action("setup.internal.remove"),
                SetupOperation::RemoveInternal {
                    operation_id: "op-7".into()
                }
            );
            let size = actions
                .iter()
                .find(|action| action.id == "setup.internal.set-target-size")
                .unwrap();
            let mut args = serde_json::Map::new();
            args.insert("gib".into(), serde_json::json!(80));
            let encoded = size.build_json(&args).unwrap();
            assert_eq!(
                decode(&encoded.type_id, &encoded.payload).unwrap(),
                Some(SetupOperation::SetInternalTargetSize { gib: 80 })
            );
            args.insert("gib".into(), serde_json::json!("eighty"));
            assert!(size.build_json(&args).is_err());
        }
    }
}
