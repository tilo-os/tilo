//! What the desktop's two layers can be asked to do, published as actions
//! on the normal action protocol: the chrome's (the overview, the menus,
//! quick settings, workspaces and strips) and the desk's (focusing,
//! closing and finding windows). A person, the palette, an agent or a test
//! reaches each the same way, by its address.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, ResourcePresentation, ResourceSpec, ResultSpec,
};

use crate::i18n::t;

/// The operation type every desktop action carries.
pub const OPERATION: &str = "tilo.desktop.command";
/// The resource of the chrome above the windows.
pub use tilo_desktop_search::CHROME_RESOURCE;
/// The resource of the desk under the windows.
pub const DESK_RESOURCE: &str = "desktop:desk";

/// One desktop action, as its payload carries it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum DesktopAction {
    /// Open or close the overview.
    ToggleOverview,
    /// Open the overview (`open`) or close it; nothing when it already is.
    Overview { open: bool },
    /// Open or close quick settings (the host's).
    ToggleQuickSettings,
    /// Open or close the Tilo menu.
    ToggleTiloMenu,
    /// Open or close the workspace menu.
    ToggleWorkspaceMenu,
    /// Go `delta` workspaces on.
    SwitchWorkspace { delta: i64 },
    /// Go `steps` strips on in the active workspace.
    StepStrip { steps: i64 },
    /// Add an empty workspace and go there; with `room`, a workspace whose
    /// shared room is `room` (joining it when this computer has none), named
    /// `name` when given.
    NewWorkspace {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Move the tab `tab` to the workspace `to` (its id or its name).
    MoveTab { tab: u64, to: String },
    /// Move the tab `tab` onto the strip `strip` (its id or its name) of
    /// the workspace it is in.
    MoveTabToStrip { tab: u64, strip: String },
    /// Name the strip `strip` (its id or its name) `name`.
    RenameStrip { strip: String, name: String },
    /// Move the strip `strip` (its id or its name) to just before `before`,
    /// or to the bottom when there is none.
    MoveStrip {
        strip: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
    },
    /// Show the workspaces this computer may join.
    JoinWorkspace,
    /// Leave the workspace `workspace` (its id; the one you are in when
    /// absent), here on this computer.
    LeaveWorkspace {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// Open the workspace's settings.
    WorkspaceSettings,
    /// Bring the tab `tab` forward and focus it.
    FocusTab { tab: u64 },
    /// Close the tab `tab` (a window's tab asks its window to close).
    CloseTab { tab: u64 },
    /// Perform the desktop shortcut `action` (its id in the shortcut table,
    /// such as `workspace_overview`), as its chord does.
    Shortcut { action: String },
    /// Choose the empty desk's launchpad card `card` (its slug, such as
    /// `notes`), or `apps` for the applications.
    Launchpad { card: String },
    /// Move focus `steps` windows along the shown strip, onto the
    /// neighbouring display past either end of the focused one.
    StepFocus { steps: i64 },
    /// Move focus to display `display`.
    FocusDisplay { display: u32 },
    /// Move the focused window `steps` displays on (back when negative).
    MoveToDisplay { steps: i64 },
    /// Open the desktop's search, with `query` typed when given.
    OpenSearch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    /// Type `query` into the open search.
    SearchFor { query: String },
    /// Choose the open search's row `row`, with `argument` the line its
    /// action asks for.
    ChooseSearchRow {
        row: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument: Option<String>,
    },
    /// Open or close the people panel: who is here.
    ToggleCollaborators,
    /// Show the person `person` in the people panel.
    ShowPerson { person: String },
}

impl DesktopAction {
    /// The action as an operation request's payload decodes, when it is one.
    #[must_use]
    pub fn decode(type_id: &str, payload: &[u8]) -> Option<Self> {
        (type_id == OPERATION)
            .then(|| serde_json::from_slice(payload).ok())
            .flatten()
    }
}

/// An immediate effect action `id` that runs `action` built from its
/// arguments.
#[expect(
    clippy::expect_used,
    reason = "a desktop action always serializes to JSON"
)]
fn action(
    id: &'static str,
    label: String,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> DesktopAction + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label,
        String::new(),
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| {
            EncodedAction::encode_operation(OPERATION, &build(args))
                .expect("a desktop action always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

fn count(args: &ActionArgs, name: &str) -> i64 {
    args.integer(name).unwrap_or(1)
}

/// The chrome's resource and its actions.
#[must_use]
pub fn chrome_resource() -> ResourceSpec {
    let resource = ResourceSpec::new(CHROME_RESOURCE, "desktop", t("desktop-actions-chrome"))
        .with_action(action(
            "toggle-overview",
            t("desktop-action-toggle-overview"),
            Vec::new(),
            |_| DesktopAction::ToggleOverview,
        ))
        .with_action(action(
            "show-overview",
            t("desktop-action-show-overview"),
            Vec::new(),
            |_| DesktopAction::Overview { open: true },
        ))
        .with_action(action(
            "hide-overview",
            t("desktop-action-hide-overview"),
            Vec::new(),
            |_| DesktopAction::Overview { open: false },
        ))
        .with_action(action(
            "toggle-quick-settings",
            t("desktop-action-toggle-quick-settings"),
            Vec::new(),
            |_| DesktopAction::ToggleQuickSettings,
        ))
        .with_action(action(
            "toggle-tilo-menu",
            t("desktop-action-toggle-tilo-menu"),
            Vec::new(),
            |_| DesktopAction::ToggleTiloMenu,
        ))
        .with_action(action(
            "toggle-workspace-menu",
            t("desktop-action-toggle-workspace-menu"),
            Vec::new(),
            |_| DesktopAction::ToggleWorkspaceMenu,
        ))
        .with_action(action(
            "switch-workspace",
            t("desktop-action-switch-workspace"),
            vec![ActionParam::required_owned(
                "delta",
                ParamType::Integer,
                t("desktop-action-param-delta"),
            )],
            |args| DesktopAction::SwitchWorkspace {
                delta: count(args, "delta"),
            },
        ))
        .with_action(action(
            "step-strip",
            t("desktop-action-step-strip"),
            vec![ActionParam::required_owned(
                "steps",
                ParamType::Integer,
                t("desktop-action-param-steps"),
            )],
            |args| DesktopAction::StepStrip {
                steps: count(args, "steps"),
            },
        ))
        .with_action(new_workspace())
        .with_action(action(
            "shortcut",
            t("desktop-action-shortcut"),
            vec![ActionParam::required_owned(
                "action",
                ParamType::Text,
                t("desktop-action-param-shortcut"),
            )],
            |args| DesktopAction::Shortcut {
                action: args.text("action").unwrap_or_default().to_owned(),
            },
        ))
        .with_action(action(
            tilo_desktop_search::SHOW_COLLABORATORS,
            t("desktop-action-toggle-collaborators"),
            Vec::new(),
            |_| DesktopAction::ToggleCollaborators,
        ))
        .with_action(action(
            tilo_desktop_search::providers::people::SHOW_PERSON,
            t("desktop-action-show-person"),
            vec![ActionParam::required_owned(
                tilo_desktop_search::providers::people::PERSON_PARAM,
                ParamType::Text,
                t("desktop-action-param-person"),
            )],
            |args| DesktopAction::ShowPerson {
                person: text(args, tilo_desktop_search::providers::people::PERSON_PARAM),
            },
        ));
    with_workspace_actions(with_strip_actions(resource))
}

/// Making a workspace: an empty one, or the one sharing the room a join was
/// granted, named as its directory lists it.
fn new_workspace() -> ActionSpec {
    action(
        "new-workspace",
        t("desktop-action-new-workspace"),
        vec![
            ActionParam::optional_owned("room", ParamType::Text, t("desktop-action-param-room")),
            ActionParam::optional_owned(
                "name",
                ParamType::Text,
                t("desktop-action-param-workspace-name"),
            ),
        ],
        |args| DesktopAction::NewWorkspace {
            room: args.text("room").map(str::to_owned),
            name: args.text("name").map(str::to_owned),
        },
    )
}

/// A window sent elsewhere, or a strip renamed or moved: what the overview
/// offers of a strip and its windows, reachable as actions.
fn with_strip_actions(resource: ResourceSpec) -> ResourceSpec {
    resource
        .with_action(action(
            "move-tab",
            t("desktop-action-move-tab"),
            vec![
                ActionParam::required_owned(
                    "tab",
                    ParamType::Integer,
                    t("desktop-action-param-tab"),
                ),
                ActionParam::required_owned(
                    "workspace",
                    ParamType::Text,
                    t("desktop-action-param-workspace"),
                ),
            ],
            |args| DesktopAction::MoveTab {
                tab: count(args, "tab").try_into().unwrap_or(0),
                to: text(args, "workspace"),
            },
        ))
        .with_action(action(
            "move-tab-to-strip",
            t("desktop-action-move-tab-to-strip"),
            vec![
                ActionParam::required_owned(
                    "tab",
                    ParamType::Integer,
                    t("desktop-action-param-tab"),
                ),
                ActionParam::required_owned(
                    "strip",
                    ParamType::Text,
                    t("desktop-action-param-strip"),
                ),
            ],
            |args| DesktopAction::MoveTabToStrip {
                tab: count(args, "tab").try_into().unwrap_or(0),
                strip: text(args, "strip"),
            },
        ))
        .with_action(action(
            "rename-strip",
            t("desktop-action-rename-strip"),
            vec![
                ActionParam::required_owned(
                    "strip",
                    ParamType::Text,
                    t("desktop-action-param-strip"),
                ),
                ActionParam::required_owned(
                    "name",
                    ParamType::Text,
                    t("desktop-action-param-strip-name"),
                ),
            ],
            |args| DesktopAction::RenameStrip {
                strip: text(args, "strip"),
                name: text(args, "name"),
            },
        ))
        .with_action(action(
            "move-strip",
            t("desktop-action-move-strip"),
            vec![
                ActionParam::required_owned(
                    "strip",
                    ParamType::Text,
                    t("desktop-action-param-strip"),
                ),
                ActionParam::optional_owned(
                    "before",
                    ParamType::Text,
                    t("desktop-action-param-before-strip"),
                ),
            ],
            |args| DesktopAction::MoveStrip {
                strip: text(args, "strip"),
                before: args.text("before").map(str::to_owned),
            },
        ))
}

/// One workspace joined, left or looked at: what the workspace menu offers
/// beside this desktop's own, reachable as actions.
fn with_workspace_actions(resource: ResourceSpec) -> ResourceSpec {
    resource
        .with_action(action(
            "join-workspace",
            t("desktop-action-join-workspace"),
            Vec::new(),
            |_| DesktopAction::JoinWorkspace,
        ))
        .with_action(action(
            "leave-workspace",
            t("desktop-action-leave-workspace"),
            vec![ActionParam::optional_owned(
                "workspace",
                ParamType::Text,
                t("desktop-action-param-workspace"),
            )],
            |args| DesktopAction::LeaveWorkspace {
                workspace: args.text("workspace").map(str::to_owned),
            },
        ))
        .with_action(action(
            "workspace-settings",
            t("desktop-action-workspace-settings"),
            Vec::new(),
            |_| DesktopAction::WorkspaceSettings,
        ))
}

/// The resource of the empty desk's launchpad.
pub const LAUNCHPAD_RESOURCE: &str = "desktop:launchpad";

/// The launchpad's resource: one action per card, by its slug, and `apps`
/// for the applications.
#[must_use]
pub fn launchpad_resource() -> ResourceSpec {
    let cards = crate::launchpad::LaunchpadAction::ALL
        .into_iter()
        .map(|card| (card.slug(), card.label()))
        .chain([("apps", t("launchpad-open-app"))]);
    cards.fold(
        ResourceSpec::new(
            LAUNCHPAD_RESOURCE,
            "desktop",
            t("desktop-actions-launchpad"),
        ),
        |resource, (slug, label)| {
            resource.with_action(action(slug, label, Vec::new(), move |_| {
                DesktopAction::Launchpad { card: slug.into() }
            }))
        },
    )
}

fn text(args: &ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().to_owned()
}

/// The resource of the open search, a child of the desk's.
pub const SEARCH_RESOURCE: &str = "desktop:search";
/// The kind of the open search's resource, and of each of its rows.
pub const SEARCH_KIND: &str = "desktop.search";
pub const SEARCH_ROW_KIND: &str = "desktop.search-row";

/// The address of the open search's row `row`.
#[must_use]
pub fn search_row_id(row: usize) -> String {
    format!("{SEARCH_RESOURCE}:row:{row}")
}

/// The kind of a window's resource.
pub const WINDOW_KIND: &str = "desktop.window";

/// The address of tab `tab`'s resource.
#[must_use]
pub fn tab_resource_id(tab: u64) -> String {
    format!("desktop:tab:{tab}")
}

/// The kind of a display's resource.
pub const DISPLAY_KIND: &str = "desktop.display";

/// The address of display `display`'s resource.
#[must_use]
pub fn display_resource_id(display: u32) -> String {
    format!("desktop:display:{display}")
}

/// The open search: what is typed, and each row it shows, which can be
/// chosen.
fn search_resource(query: &str, rows: &[tilo_desktop_surface::SearchRow]) -> ResourceSpec {
    let rows = rows.iter().enumerate().map(|(at, row)| {
        let index = u64::try_from(at).unwrap_or(u64::MAX);
        let mut spec = ResourceSpec::new(search_row_id(at), SEARCH_ROW_KIND, row.title.clone())
            .with_action(action(
                "choose",
                t("desktop-action-choose-search-row"),
                vec![ActionParam::optional_owned(
                    "argument",
                    ParamType::Text,
                    t("desktop-action-param-argument"),
                )],
                move |args| DesktopAction::ChooseSearchRow {
                    row: index,
                    argument: args.text("argument").map(str::to_owned),
                },
            ));
        // What the row is sits under its name, as the row's second line.
        if let Some(subtitle) = &row.subtitle {
            spec = spec.with_presentation(ResourcePresentation {
                detail: Some(subtitle.clone()),
                ..ResourcePresentation::default()
            });
        }
        spec
    });
    let label = if query.is_empty() {
        t("desktop-actions-search")
    } else {
        query.to_owned()
    };
    ResourceSpec::new(SEARCH_RESOURCE, SEARCH_KIND, label)
        .with_action(action(
            "search-for",
            t("desktop-action-search-for"),
            vec![ActionParam::required_owned(
                "query",
                ParamType::Text,
                t("desktop-action-param-query"),
            )],
            |args| DesktopAction::SearchFor {
                query: text(args, "query"),
            },
        ))
        .with_children(rows)
}

/// The desk's resource and its actions, with each tab of `tabs`
/// (`(tab, title)`) a resource of its own that can be focused and closed,
/// across several displays each of `displays` (in reading order) one that
/// can be focused, and the open search (its query and rows), when open.
#[must_use]
pub fn desk_resource(
    tabs: &[(u64, String)],
    displays: &[u32],
    search: Option<(&str, &[tilo_desktop_surface::SearchRow])>,
) -> ResourceSpec {
    let screens = displays.iter().enumerate().map(|(index, display)| {
        let display = *display;
        let number = index.saturating_add(1).to_string();
        ResourceSpec::new(
            display_resource_id(display),
            DISPLAY_KIND,
            crate::i18n::t_args(
                "desktop-display-name",
                &[("number", number.as_str().into())],
            ),
        )
        .with_action(action(
            "focus",
            t("desktop-action-focus-display"),
            Vec::new(),
            move |_| DesktopAction::FocusDisplay { display },
        ))
    });
    let windows = tabs.iter().map(|(id, title)| {
        let id = *id;
        ResourceSpec::new(tab_resource_id(id), WINDOW_KIND, title.clone())
            .with_action(action(
                "focus",
                t("desktop-action-focus-tab"),
                Vec::new(),
                move |_| DesktopAction::FocusTab { tab: id },
            ))
            .with_action(action(
                "close",
                t("desktop-action-close-tab"),
                Vec::new(),
                move |_| DesktopAction::CloseTab { tab: id },
            ))
    });
    let search = search.map(|(query, rows)| search_resource(query, rows));
    ResourceSpec::new(DESK_RESOURCE, "desktop", t("desktop-actions-desk"))
        .with_action(action(
            "open-search",
            t("desktop-action-open-search"),
            vec![ActionParam::optional_owned(
                "query",
                ParamType::Text,
                t("desktop-action-param-query"),
            )],
            |args| DesktopAction::OpenSearch {
                query: args.text("query").map(str::to_owned),
            },
        ))
        .with_action(action(
            "step-focus",
            t("desktop-action-step-focus"),
            vec![ActionParam::required_owned(
                "steps",
                ParamType::Integer,
                t("desktop-action-param-window-steps"),
            )],
            |args| DesktopAction::StepFocus {
                steps: count(args, "steps"),
            },
        ))
        .with_action(action(
            "move-to-display",
            t("desktop-action-move-to-display"),
            vec![ActionParam::required_owned(
                "steps",
                ParamType::Integer,
                t("desktop-action-param-display-steps"),
            )],
            |args| DesktopAction::MoveToDisplay {
                steps: count(args, "steps"),
            },
        ))
        .with_children(windows.chain(screens).chain(search))
}
