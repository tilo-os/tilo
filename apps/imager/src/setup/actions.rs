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
use super::strings;

pub const SETUP_RESOURCE_ID: &str = "imager.setup";
pub const SETUP_OPERATION_TYPE: &str = "imager.setup.operation";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SetupOperation {
    ChooseExperience { experience: String },
    SetCommonMode { mode: String },
    SetFolderMode { folder: String, mode: String },
    ClearFolderMode { folder: String },
    RemoveFolder { folder: String },
    SelectSuggestion { suggestion: String },
    ChooseFolder,
    ToggleHelp,
    Next,
    Back,
    OpenImports,
    InstallDesktop,
    CancelDesktopDownload,
    RetryDesktop,
    StartInternalDownload,
    PauseInternalDownload,
    ResumeInternalDownload,
    CancelInternalDownload,
    DiscardInternalDownload,
    ReleaseInternalDownload,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FolderProjection {
    pub identity: String,
    pub index: usize,
    pub mode: &'static str,
    pub overridden: bool,
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SuggestionProjection {
    pub identity: String,
    pub name: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DesktopProjection {
    pub phase: &'static str,
    pub status: String,
    pub install: bool,
    pub cancel_download: bool,
    pub retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct InternalDownloadProjection {
    pub phase: Option<&'static str>,
    pub status: String,
    pub start: bool,
    pub pause: bool,
    pub resume: bool,
    pub cancel: bool,
    pub discard: bool,
    pub release: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SetupProjection {
    pub page: &'static str,
    pub selection: SelectionProjection,
    pub review: ReviewProjection,
    pub availability: AvailabilityProjection,
    pub desktop: DesktopProjection,
    pub internal_download: InternalDownloadProjection,
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
        _ => Err(strings::t_args(
            "imager-setup-error-unknown-experience",
            &[("value", value.into())],
        )),
    }
}

pub fn parse_mode(value: &str) -> Result<FolderMode, String> {
    match value {
        "copy" => Ok(FolderMode::Copy),
        "connected" => Ok(FolderMode::Connected),
        "later" => Ok(FolderMode::Later),
        _ => Err(strings::t_args(
            "imager-setup-error-unknown-mode",
            &[("value", value.into())],
        )),
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
    serde_json::from_slice(payload).map(Some).map_err(|error| {
        strings::t_args(
            "imager-setup-error-invalid-operation",
            &[("error", error.to_string().into())],
        )
    })
}

fn result_spec() -> ResultSpec {
    ResultSpec::Json {
        schema: serde_json::json!({ "type": "object" }),
    }
}

fn immediate(id: &str, label: impl Into<String>, operation: SetupOperation) -> ActionSpec {
    let label = label.into();
    ActionSpec::operation(
        id,
        label.clone(),
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

fn with_availability(
    action: ActionSpec,
    available: bool,
    code: &str,
    reason: impl Into<String>,
) -> ActionSpec {
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
        strings::t("imager-setup-action-set-folder-mode"),
        strings::t("imager-setup-placeholder-folder-mode"),
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "mode",
                ParamType::Text,
                strings::t("imager-setup-param-folder-mode"),
            )
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
        strings::t("imager-setup-error-folder-choices-closed"),
    );
    let clear_mode = with_availability(
        immediate(
            "use-common-mode",
            strings::t("imager-setup-action-use-common-mode"),
            SetupOperation::ClearFolderMode {
                folder: identity.clone(),
            },
        ),
        edits_available,
        "folder_choices_not_open",
        strings::t("imager-setup-error-folder-choices-closed"),
    );
    let remove = with_availability(
        immediate(
            "remove",
            strings::t("imager-setup-action-remove-folder"),
            SetupOperation::RemoveFolder {
                folder: identity.clone(),
            },
        ),
        edits_available,
        "folder_choices_not_open",
        strings::t("imager-setup-error-folder-choices-closed"),
    );

    Resource::new(
        format!("{SETUP_RESOURCE_ID}.folder.{identity}"),
        "imager.setup.folder",
        strings::t_args(
            "imager-setup-resource-selected-folder",
            &[("number", (folder.index + 1).into())],
        ),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            if folder.overridden {
                "imager-setup-detail-folder-override"
            } else {
                "imager-setup-detail-folder-common"
            },
            &[("mode", folder.mode.into())],
        )),
        badge: Some(strings::t_args(
            "imager-setup-badge-folder",
            &[("number", (folder.index + 1).into())],
        )),
    })
    .with_action(set_mode)
    .with_action(clear_mode)
    .with_action(remove)
}

fn suggestion_resource(suggestion: &SuggestionProjection, choices_open: bool) -> Resource {
    let available = choices_open && suggestion.available;
    let reason = if !choices_open {
        strings::t("imager-setup-error-folder-choices-closed")
    } else {
        suggestion
            .unavailable_reason
            .clone()
            .unwrap_or_else(|| strings::t("imager-setup-error-suggestion-unavailable"))
    };
    let select = with_availability(
        immediate(
            "select",
            strings::t("imager-setup-action-select-suggestion"),
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
        reason,
    );
    Resource::new(
        format!("{SETUP_RESOURCE_ID}.suggestion.{}", suggestion.identity),
        "imager.setup.suggestion",
        strings::t_args(
            "imager-setup-resource-suggested-folder",
            &[("folder", suggestion.name.clone().into())],
        ),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(if suggestion.available {
            strings::t("imager-setup-suggestion-available")
        } else {
            strings::t("imager-setup-suggestion-unavailable")
        }),
        badge: Some(strings::t("imager-setup-badge-suggestion")),
    })
    .with_action(select)
}

fn desktop_resource(desktop: &DesktopProjection) -> Resource {
    let install = with_availability(
        immediate(
            "install",
            strings::t("imager-setup-action-install-desktop"),
            SetupOperation::InstallDesktop,
        ),
        desktop.install,
        "desktop_install_unavailable",
        strings::t("imager-setup-error-desktop-install-unavailable"),
    );
    let cancel = with_availability(
        immediate(
            "cancel-download",
            strings::t("imager-setup-action-cancel-desktop-download"),
            SetupOperation::CancelDesktopDownload,
        ),
        desktop.cancel_download,
        "desktop_download_not_active",
        strings::t("imager-setup-error-desktop-download-not-active"),
    );
    let retry = with_availability(
        immediate(
            "retry",
            strings::t("imager-setup-action-retry-desktop"),
            SetupOperation::RetryDesktop,
        ),
        desktop.retry,
        "desktop_retry_unavailable",
        strings::t("imager-setup-error-desktop-retry-unavailable"),
    );
    Resource::new(
        format!("{SETUP_RESOURCE_ID}.desktop"),
        "imager.setup.desktop",
        strings::t("imager-setup-resource-desktop"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-setup-detail-desktop",
            &[
                ("phase", desktop.phase.into()),
                ("status", desktop.status.clone().into()),
            ],
        )),
        badge: Some(strings::t("imager-setup-badge-desktop")),
    })
    .with_action(install)
    .with_action(cancel)
    .with_action(retry)
}

fn internal_download_resource(download: &InternalDownloadProjection) -> Resource {
    let action = |id, label, operation, available, code, reason| {
        with_availability(
            immediate(id, strings::t(label), operation),
            available,
            code,
            strings::t(reason),
        )
    };
    Resource::new(
        format!("{SETUP_RESOURCE_ID}.internal-download"),
        "imager.setup.internal-download",
        strings::t("imager-setup-resource-internal-download"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-setup-detail-internal-download",
            &[
                ("phase", download.phase.unwrap_or("not_started").into()),
                ("status", download.status.clone().into()),
            ],
        )),
        badge: Some(strings::t("imager-setup-badge-download")),
    })
    .with_action(action(
        "start",
        "imager-setup-action-start-download",
        SetupOperation::StartInternalDownload,
        download.start,
        "download_start_unavailable",
        "imager-setup-error-download-start-unavailable",
    ))
    .with_action(action(
        "pause",
        "imager-setup-action-pause-download",
        SetupOperation::PauseInternalDownload,
        download.pause,
        "download_pause_unavailable",
        "imager-setup-error-download-pause-unavailable",
    ))
    .with_action(action(
        "resume",
        "imager-setup-action-resume-download",
        SetupOperation::ResumeInternalDownload,
        download.resume,
        "download_resume_unavailable",
        "imager-setup-error-download-resume-unavailable",
    ))
    .with_action(action(
        "cancel",
        "imager-setup-action-cancel-download",
        SetupOperation::CancelInternalDownload,
        download.cancel,
        "download_cancel_unavailable",
        "imager-setup-error-download-cancel-unavailable",
    ))
    .with_action(action(
        "discard",
        "imager-setup-action-discard-download",
        SetupOperation::DiscardInternalDownload,
        download.discard,
        "download_discard-unavailable",
        "imager-setup-error-download-discard-unavailable",
    ))
    .with_action(action(
        "release",
        "imager-setup-action-release-download",
        SetupOperation::ReleaseInternalDownload,
        download.release,
        "download_release_unavailable",
        "imager-setup-error-download-release-unavailable",
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
        strings::t("imager-setup-action-choose-experience"),
        strings::t("imager-setup-placeholder-experience"),
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "experience",
                ParamType::Text,
                strings::t("imager-setup-param-experience"),
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
        strings::t("imager-setup-error-no-experience"),
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
        strings::t("imager-setup-action-set-common-mode"),
        strings::t("imager-setup-placeholder-folder-mode"),
        OperationKind::View,
        SETUP_OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "mode",
                ParamType::Text,
                strings::t("imager-setup-param-common-folder-mode"),
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
        strings::t("imager-setup-error-folder-choices-closed"),
    );

    let help = with_availability(
        immediate(
            "toggle-help",
            strings::t("imager-setup-action-toggle-help"),
            SetupOperation::ToggleHelp,
        ),
        state.availability.help,
        "help_not_on_page",
        strings::t("imager-setup-error-help-not-on-page"),
    );
    let next = with_availability(
        immediate(
            "next",
            strings::t("imager-setup-action-next"),
            SetupOperation::Next,
        ),
        state.availability.next,
        "setup_cannot_continue",
        strings::t("imager-setup-error-cannot-continue"),
    );
    let back = with_availability(
        immediate(
            "back",
            strings::t("imager-setup-action-back"),
            SetupOperation::Back,
        ),
        state.availability.back,
        "setup_at_start",
        strings::t("imager-setup-error-at-start"),
    );

    let choose_folder = with_availability(
        immediate(
            "choose-folder",
            strings::t("imager-setup-action-choose-folder"),
            SetupOperation::ChooseFolder,
        ),
        state.availability.choose_folder,
        "folder_picker_unavailable",
        strings::t("imager-setup-error-picker-unavailable"),
    );

    let folder_resources = state
        .selection
        .folders
        .iter()
        .map(|folder| folder_resource(folder, &mode_choices, state.availability.folder_edits))
        .collect::<Vec<_>>();
    let suggestion_resources = state
        .selection
        .suggestions
        .iter()
        .map(|suggestion| suggestion_resource(suggestion, state.page == "folders"))
        .collect::<Vec<_>>();
    let selection = Resource::new(
        "imager.setup.selection",
        "imager.setup.selection",
        strings::t("imager-setup-resource-selection"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-setup-detail-selection",
            &[
                (
                    "experience",
                    state.selection.experience.unwrap_or("none").into(),
                ),
                ("common_mode", state.selection.common_mode.into()),
                ("folder_count", state.selection.folder_count.into()),
                (
                    "active_folder_count",
                    state.selection.active_folder_count.into(),
                ),
                ("help", flag(state.selection.help_open).into()),
                ("imports", flag(state.selection.imports_open).into()),
            ],
        )),
        badge: Some(strings::t("imager-setup-badge-selection")),
    })
    .with_children(folder_resources);
    let review = Resource::new(
        "imager.setup.review",
        "imager.setup.review",
        strings::t("imager-setup-resource-review"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-setup-detail-review",
            &[
                ("ready", flag(state.review.ready).into()),
                ("page", state.page.into()),
            ],
        )),
        badge: Some(strings::t("imager-setup-badge-review")),
    });
    let availability = Resource::new(
        "imager.setup.availability",
        "imager.setup.availability",
        strings::t("imager-setup-resource-availability"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-setup-detail-availability",
            &[
                ("desktop", flag(state.availability.desktop).into()),
                ("full_os", flag(state.availability.full_os).into()),
                ("connected", flag(state.availability.connected).into()),
                ("next", flag(state.availability.next).into()),
                ("back", flag(state.availability.back).into()),
                (
                    "folder_policies",
                    flag(state.availability.folder_policies).into(),
                ),
                ("folder_edits", flag(state.availability.folder_edits).into()),
                ("help", flag(state.availability.help).into()),
                ("open_imports", flag(state.availability.open_imports).into()),
            ],
        )),
        badge: Some(strings::t("imager-setup-badge-availability")),
    });

    let desktop = desktop_resource(&state.desktop);
    let internal_download = internal_download_resource(&state.internal_download);

    Resource::new(
        SETUP_RESOURCE_ID,
        "imager.setup",
        strings::t("imager-setup-resource-root"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-setup-detail-root",
            &[
                ("page", state.page.into()),
                (
                    "experience",
                    state.selection.experience.unwrap_or("none").into(),
                ),
                ("common_mode", state.selection.common_mode.into()),
                ("folder_count", state.selection.folder_count.into()),
                ("review", flag(state.review.ready).into()),
                ("desktop", flag(state.availability.desktop).into()),
                ("full_os", flag(state.availability.full_os).into()),
                ("connected", flag(state.availability.connected).into()),
                ("next", flag(state.availability.next).into()),
                ("back", flag(state.availability.back).into()),
            ],
        )),
        badge: Some(strings::t(match state.page {
            "experience" => "imager-setup-badge-page-experience",
            "folders" => "imager-setup-badge-page-folders",
            "review" => "imager-setup-badge-page-review",
            _ => "imager-setup-badge-page-setup",
        })),
    })
    .with_action(choose_experience)
    .with_action(common_mode)
    .with_action(choose_folder)
    .with_action(help)
    .with_action(next)
    .with_action(back)
    .with_action(immediate(
        "open-imports",
        strings::t("imager-setup-action-open-imports"),
        SetupOperation::OpenImports,
    ))
    .with_children([selection, review, availability, desktop, internal_download])
    .with_children(suggestion_resources)
}

const fn flag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
