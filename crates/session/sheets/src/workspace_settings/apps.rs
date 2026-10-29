//! The Apps & Files tab: which app opens each kind of file and link here.
//! Each kind has the workspace's choice, which its admins make for
//! everyone, and the person's own, which may follow the workspace's, their
//! personal default or name an app of its own. A press moves either to the
//! next app that can open the kind.

use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{Button, Column, ControlSize, Element, EmptyState, ListRow, Row, Section};

use super::agents::{Intro, column, intro};
use super::general::caption;
use super::{Parts, Settings, WorkspaceSettingsEffect};

/// One kind of file or link, with both choices for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppRow {
    /// What the kind is asked for by.
    pub resource: String,
    pub label: String,
    /// The workspace's choice, as the person reads it.
    pub workspace: String,
    /// The person's own choice.
    pub yours: String,
}

/// Everything the tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Apps {
    pub intro: Intro,
    pub header: String,
    /// What the workspace's and the person's choices are called.
    pub workspace_choice: String,
    pub your_choice: String,
    pub rows: Vec<AppRow>,
    /// Whether the person may make the workspace's choices.
    pub manage: bool,
    /// What the tab says when no app opens anything: title and detail.
    pub empty: (String, String),
    pub footnote: Option<String>,
}

/// The tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let apps = &settings.apps;
    let mut groups = vec![intro(&apps.intro, Icon::LayoutGrid)];
    if apps.rows.is_empty() {
        let (title, detail) = &apps.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::LayoutGrid)
                .with_message(detail.clone())
                .inline(),
        ));
    } else {
        groups.push(Box::new(Section::titled(
            apps.header.clone(),
            apps.rows
                .iter()
                .map(|row| app_row(apps, row, parts))
                .collect(),
        )));
    }
    if let Some(note) = &apps.footnote {
        groups.push(caption(note));
    }
    column(groups)
}

/// One kind: its name, then the workspace's choice and the person's, each
/// a button naming the app chosen, captioned with whose choice it is.
fn app_row(apps: &Apps, row: &AppRow, parts: &Parts) -> Element {
    let choice = |id: String, whose: &str, chosen: &str, effect: WorkspaceSettingsEffect| {
        let press = parts.clone();
        Button::new(&id, chosen)
            .with_accessible_name(&format!("{}, {whose}: {chosen}", row.label))
            .tooltip(whose.to_owned())
            .control_size(ControlSize::Small)
            .on_click(move || press.ask(effect.clone()))
    };
    let captioned = |whose: &str, button: Button| -> Element {
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(caption(whose))
                .push(Box::new(button)),
        )
    };
    let workspace = choice(
        format!("ws-apps-workspace-{}", row.resource),
        &apps.workspace_choice,
        &row.workspace,
        WorkspaceSettingsEffect::NextWorkspaceDefault {
            resource: row.resource.clone(),
        },
    )
    .with_enabled(apps.manage);
    let yours = choice(
        format!("ws-apps-yours-{}", row.resource),
        &apps.your_choice,
        &row.yours,
        WorkspaceSettingsEffect::NextPersonalDefault {
            resource: row.resource.clone(),
        },
    );
    Box::new(
        ListRow::new(format!("ws-apps-row-{}", row.resource), row.label.clone()).trailing(
            Box::new(
                Row::new()
                    .with_rhythm(Rhythm::Related)
                    .align_start()
                    .push(captioned(&apps.workspace_choice, workspace))
                    .push(captioned(&apps.your_choice, yours)),
            ),
        ),
    )
}

/// The tab as the workspace's owner sees it, for captures.
pub(super) fn sample() -> Apps {
    let row = |resource: &str, label: &str, workspace: &str, yours: &str| AppRow {
        resource: resource.to_owned(),
        label: label.to_owned(),
        workspace: workspace.to_owned(),
        yours: yours.to_owned(),
    };
    Apps {
        intro: Intro {
            title: "Default apps".to_owned(),
            help: "Which app opens each kind of file and link in this workspace.".to_owned(),
            authority: "1 custom rule".to_owned(),
        },
        header: "File types".to_owned(),
        workspace_choice: "Workspace".to_owned(),
        your_choice: "You".to_owned(),
        rows: vec![
            row("ext:md", ".md files", "Notes", "Use the workspace's choice"),
            row(
                "scheme:mailto",
                "mailto: links",
                "Follow each person's default",
                "Mail",
            ),
        ],
        manage: true,
        empty: (
            "No apps open files yet".to_owned(),
            "Apps that open files show here once installed.".to_owned(),
        ),
        footnote: None,
    }
}
