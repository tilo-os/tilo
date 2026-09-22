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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SetupProjection {
    pub page: &'static str,
    pub selection: SelectionProjection,
    pub review: ReviewProjection,
    pub availability: AvailabilityProjection,
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
    })
    .with_action(select)
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
        .with_children([selection, suggestions, review, availability])
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

const fn open_closed(value: bool) -> &'static str {
    if value { "open" } else { "closed" }
}
