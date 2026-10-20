//! The ordinary portal chooser, reachable through the canonical action registry.
use crate::shell::file_picker_overlay::actions::{PickerActions, Subject};
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{
    InvocationDisposition, ResourceKind, SearchContext, SearchProvider, SearchResult,
};
use std::rc::Rc;
use ui::widget::file_picker::semantic::PickerOperation;

pub const PROVIDER_ID: &str = "file-picker";

pub(crate) fn authorized(
    state: &crate::shell::chrome_state::ChromeState,
    subject: &Subject,
) -> bool {
    !state.session_locked()
        && crate::shell::overlay_host::is_active_workspace_owner(state)
        && state
            .tree()
            .desktop
            .active_workspace()
            .is_some_and(|workspace| workspace.id == subject.workspace)
}

pub struct FilePickerProvider {
    actions: PickerActions,
    ranker: ui::search::ListIndex,
}
impl FilePickerProvider {
    pub(crate) fn new(actions: PickerActions) -> Self {
        Self {
            actions,
            ranker: ui::search::ListIndex::default(),
        }
    }
}

fn operation(name: &str, args: &ActionArgs) -> Result<PickerOperation, ActionError> {
    let entry = || {
        args.text("entry")
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map(str::to_owned)
            .ok_or_else(|| ActionError::Explained(t("file-picker-entry-unavailable")))
    };
    Ok(match name {
        "inspect" => PickerOperation::Inspect {
            page: usize::try_from(args.integer("page").unwrap_or(0))
                .map_err(|_| ActionError::Explained(t("file-picker-page-unavailable")))?,
        },
        "select" => PickerOperation::Select { entry: entry()? },
        "navigate" => PickerOperation::Navigate { entry: entry()? },
        "up" => PickerOperation::Up,
        "confirm" => PickerOperation::Confirm,
        "cancel" => PickerOperation::Cancel,
        "filename" => PickerOperation::SetFilename {
            filename: args.text("filename").unwrap_or_default().to_owned(),
        },
        "replace" => PickerOperation::Replace,
        "keep-existing" => PickerOperation::KeepExisting,
        "next-filter" => PickerOperation::NextFilter,
        _ => return Err(ActionError::Explained(t("file-picker-expired"))),
    })
}

fn parameters(name: &str) -> Vec<ActionParam> {
    let mut params = Vec::new();
    if name == "inspect" {
        params.push(ActionParam::optional_owned(
            "page",
            ParamType::Integer,
            t("file-picker-page-param"),
        ));
    }
    if !matches!(name, "inspect" | "cancel") {
        params.push(ActionParam::required_owned(
            "state",
            ParamType::Text,
            t("file-picker-state-param"),
        ));
    }
    if matches!(name, "select" | "navigate") {
        params.push(ActionParam::required_owned(
            "entry",
            ParamType::Text,
            t("file-picker-entry-param"),
        ));
    }
    if name == "filename" {
        params.push(ActionParam::required_owned(
            "filename",
            ParamType::Text,
            t("file-picker-filename-param"),
        ));
    }
    let grants = matches!(name, "confirm" | "replace");
    if grants {
        params.push(ActionParam::required_owned(
            "confirm",
            ParamType::Boolean,
            t("file-picker-confirm-param"),
        ));
    }
    params
}

/// What a picker operation promises: one that grants access to a file asks
/// first and cannot be taken back; every one answers with the picker's state.
fn contract(grants: bool) -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Outside,
        risk: if grants {
            tilo_actions::RiskLevel::Destructive
        } else {
            tilo_actions::RiskLevel::None
        },
        confirmation: if grants {
            tilo_actions::ConfirmationPolicy::Confirm
        } else {
            tilo_actions::ConfirmationPolicy::None
        },
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        execution: tilo_actions::ExecutionMode::Background { cancellable: false },
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type":"object", "required":["request_id", "application", "state", "completed", "entries"],
                "properties": {"request_id":{"type":"string"}, "application":{"type":"string"},
                    "state":{"type":"string"}, "completed":{"type":"boolean"}, "entries":{"type":"array"}}}),
        },
        ..Default::default()
    }
}

impl SearchProvider for FilePickerProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "File chooser"
    }
    fn display_name(&self) -> String {
        t("search-provider-file-picker")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let Some(subject) = self
            .actions
            .subject()
            .filter(|subject| authorized(ctx.chrome, subject))
        else {
            return Vec::new();
        };
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let rows: Vec<_> = [
            "inspect",
            "select",
            "navigate",
            "up",
            "confirm",
            "cancel",
            "filename",
            "replace",
            "keep-existing",
            "next-filter",
        ]
        .into_iter()
        .map(|name| {
            let params = parameters(name);
            let grants = matches!(name, "confirm" | "replace");
            let actions = self.actions.clone();
            let subject = subject.clone();
            let expected = subject.id;
            SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: if name == "inspect" {
                    tilo_actions::OperationKind::View
                } else {
                    tilo_actions::OperationKind::Effect
                },
                provider_id: PROVIDER_ID.into(),
                id: format!("{name}@{expected}"),
                title: t(&format!("file-picker-{name}")),
                subtitle: Some(subject.app.clone()),
                badge: Some(self.display_name()),
                score: 0,
                params,
                contract: Some(contract(grants)),
                on_open: Rc::new(|_, _, _, _| {
                    Err(ActionError::Explained(t("file-picker-declared-operation")))
                }),
                on_invoke: None,
                on_begin_invoke: Some(Rc::new(move |_, _, chrome, args| {
                    if !authorized(&chrome.borrow(), &subject) {
                        return Err(ActionError::Explained(t("file-picker-owner-required")));
                    }
                    if grants && args.boolean("confirm") != Some(true) {
                        return Err(ActionError::Explained(t("file-picker-confirm-param")));
                    }
                    let state = args.text("state").map(str::to_owned);
                    if state.as_ref().is_some_and(|value| {
                        value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    }) {
                        return Err(ActionError::Explained(t("file-picker-expired")));
                    }
                    actions
                        .begin(expected, state, operation(name, args)?)
                        .map(InvocationDisposition::Pending)
                })),
            }
        })
        .collect();
        let fields = rows
            .iter()
            .map(|row| {
                vec![
                    (row.title.clone(), ui::search::FieldWeight::TITLE),
                    (row.id.clone(), ui::search::FieldWeight::IDENTITY),
                    (PROVIDER_ID.into(), ui::search::FieldWeight::IDENTITY),
                ]
            })
            .collect::<Vec<_>>();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle.trim(), rows, &fields)
    }
}
