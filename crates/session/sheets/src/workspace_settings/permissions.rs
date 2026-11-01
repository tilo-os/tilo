//! The Privacy & Security tab: what the workspace lets its people, agents
//! and apps do beyond their roles, grouped by who holds it, each with its
//! Revoke, and a composer giving one more: who, what it allows, and for how
//! long.
//!
//! The composer's choices are the sheet's own until the person grants; the
//! grant names all three, so whoever asks for it says exactly what it gives.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, ControlSize, Element, EmptyState, ListRow, MenuItem, Row, Section,
    Segment, Text, ToggleGroup,
};

use super::agents::{Intro, column, intro};
use super::general::{button, buttons, caption};
use super::{Confirm, Parts, Settings, WorkspaceSettingsEffect};

/// One thing a list offers to choose, by what it is asked for by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub key: String,
    pub label: String,
}

/// The composer giving one more permission.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    pub header: String,
    /// Who the permission is for: the people, agents and apps it may name,
    /// by keys `member:…`, `agent:…` and `app:…`.
    pub who: String,
    /// What the choice of who is called while it names an agent.
    pub who_agent: String,
    pub subjects: Vec<Choice>,
    /// What the composer says when there is no one to give it to.
    pub nobody: String,
    pub what: String,
    pub permissions: Vec<Choice>,
    pub how_long: String,
    pub expiries: Vec<Choice>,
    /// The expiry a new permission starts with.
    pub default_expiry: String,
    pub grant: String,
}

/// One permission the workspace gives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permission {
    /// What it is revoked by.
    pub key: String,
    /// Who holds it.
    pub holder: String,
    /// What it allows, and who gave it.
    pub detail: String,
    /// How long it lasts.
    pub expiry: String,
}

/// The permissions held by one kind of holder: people, agents, apps, apps
/// acting on other apps, roles, and everything else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holders {
    pub title: String,
    pub permissions: Vec<Permission>,
}

/// Everything the tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    pub intro: Intro,
    /// The saved roles' button, which opens their actions.
    pub saved_roles: String,
    /// Whether the person may give and revoke permissions.
    pub manage: bool,
    pub composer: Composer,
    pub sections: Vec<Holders>,
    pub revoke: String,
    /// What the tab says while nothing is given: title and detail.
    pub empty: (String, String),
    /// Why the controls do not take input, for someone who may not use them.
    pub footnote: Option<String>,
    /// The question revoking a permission asks.
    pub revoke_decision: super::Decision,
}

/// What the composer has chosen so far: the sheet's own until it grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Draft {
    pub subject: Option<String>,
    pub permission: Option<String>,
    pub expiry: Option<String>,
}

impl Draft {
    /// What the draft names among what `composer` offers now: a choice that
    /// went away falls back to the first one there is.
    fn resolved(&self, composer: &Composer) -> (Option<Choice>, Option<Choice>, String) {
        let pick = |chosen: &Option<String>, choices: &[Choice]| {
            chosen
                .as_ref()
                .and_then(|key| choices.iter().find(|choice| &choice.key == key))
                .or_else(|| choices.first())
                .cloned()
        };
        let expiry = self
            .expiry
            .clone()
            .filter(|key| composer.expiries.iter().any(|choice| &choice.key == key))
            .unwrap_or_else(|| composer.default_expiry.clone());
        (
            pick(&self.subject, &composer.subjects),
            pick(&self.permission, &composer.permissions),
            expiry,
        )
    }
}

/// The tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let permissions = &settings.permissions;
    let mut groups: Vec<Element> = vec![
        intro(&permissions.intro, Icon::Shield),
        buttons(vec![
            button(
                parts,
                "ws-permissions-roles",
                &permissions.saved_roles,
                || WorkspaceSettingsEffect::SavedRoles,
            )
            .with_enabled(permissions.manage),
        ]),
        composer(permissions, parts),
    ];
    if permissions.sections.is_empty() {
        let (title, detail) = &permissions.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::Shield)
                .with_message(detail.clone())
                .inline(),
        ));
    }
    for holders in &permissions.sections {
        groups.push(Box::new(Section::titled(
            holders.title.clone(),
            holders
                .permissions
                .iter()
                .map(|permission| row(permissions, permission, parts))
                .collect(),
        )));
    }
    if let Some(note) = &permissions.footnote {
        groups.push(caption(note));
    }
    column(groups)
}

/// Who, what and for how long, then Grant.
fn composer(permissions: &Permissions, parts: &Parts) -> Element {
    let composer = &permissions.composer;
    let (subject, permission, expiry) = parts.local.get().draft.resolved(composer);
    let mut items: Vec<Element> = Vec::new();
    let mut pickers = Row::new().with_rhythm(Rhythm::Related).align_start();
    match &subject {
        Some(subject) => {
            let who = if subject.key.starts_with("agent:") {
                &composer.who_agent
            } else {
                &composer.who
            };
            pickers = pickers.push(picker(
                "ws-grant-who",
                who,
                subject,
                &composer.subjects,
                permissions.manage,
                parts,
                |draft, key| draft.subject = Some(key),
            ));
        }
        None => items.push(caption(&composer.nobody)),
    }
    if let Some(permission) = &permission {
        pickers = pickers.push(picker(
            "ws-grant-what",
            &composer.what,
            permission,
            &composer.permissions,
            permissions.manage,
            parts,
            |draft, key| draft.permission = Some(key),
        ));
    }
    items.push(Box::new(pickers));
    let selected = composer
        .expiries
        .iter()
        .position(|choice| choice.key == expiry)
        .unwrap_or_default();
    let keys: Vec<String> = composer
        .expiries
        .iter()
        .map(|choice| choice.key.clone())
        .collect();
    let choose = parts.clone();
    items.push(caption(&composer.how_long));
    items.push(Box::new(
        ToggleGroup::new(
            composer
                .expiries
                .iter()
                .map(|choice| Segment::new(choice.label.clone()))
                .collect(),
            selected,
        )
        .with_id("ws-grant-expiry")
        .with_accessible_name(composer.how_long.clone())
        .with_enabled(permissions.manage)
        .on_view("ws-grant-expiry", move |index| {
            if let Some(key) = keys.get(index) {
                let key = key.clone();
                choose.local.update(|local| local.draft.expiry = Some(key));
            }
        }),
    ));
    let grant = parts.clone();
    let named = subject
        .as_ref()
        .zip(permission.as_ref())
        .map(|(subject, permission)| (subject.key.clone(), permission.key.clone()));
    let can_grant = permissions.manage && named.is_some();
    items.push(buttons(vec![
        Button::new("ws-grant", &composer.grant)
            .role(ButtonRole::Primary)
            .with_enabled(can_grant)
            .on_effect(move || {
                if let Some((subject, permission)) = named.clone() {
                    grant.ask(WorkspaceSettingsEffect::Grant {
                        subject,
                        permission,
                        expiry: expiry.clone(),
                    });
                }
            }),
    ]));
    Box::new(Section::titled(composer.header.clone(), items))
}

/// A button saying what is chosen, whose menu chooses another.
fn picker(
    id: &str,
    caption_text: &str,
    chosen: &Choice,
    choices: &[Choice],
    enabled: bool,
    parts: &Parts,
    set: fn(&mut Draft, String),
) -> Element {
    let items = choices
        .iter()
        .map(|choice| {
            let (choose, key) = (parts.clone(), choice.key.clone());
            MenuItem::action(choice.label.clone(), move || {
                let key = key.clone();
                choose.local.update(|local| set(&mut local.draft, key));
            })
            .with_checked(choice.key == chosen.key)
        })
        .collect();
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(caption(caption_text))
            .push(Box::new(
                Button::new(id, &chosen.label)
                    .with_accessible_name(&format!("{caption_text}: {}", chosen.label))
                    .with_enabled(enabled)
                    .with_menu(items),
            )),
    )
}

/// One permission: who holds it, what it allows and who gave it, how long
/// it lasts, and Revoke, which asks first.
fn row(permissions: &Permissions, permission: &Permission, parts: &Parts) -> Element {
    let confirm = parts.clone();
    let (grant, holder) = (permission.key.clone(), permission.holder.clone());
    Box::new(
        ListRow::new(
            format!("ws-permission-{}", permission.key),
            permission.holder.clone(),
        )
        .avatar(permission.holder.clone(), permission.holder.clone())
        .secondary(permission.detail.clone())
        .two_line_secondary()
        .trailing(Box::new(
            Row::new()
                .with_rhythm(Rhythm::Related)
                .align_start()
                .push(Box::new(
                    Text::new(&permission.expiry)
                        .with_role(TextRole::Caption)
                        .dimmed(),
                ))
                .push(Box::new(
                    Button::new(
                        &format!("ws-permission-revoke-{}", permission.key),
                        &permissions.revoke,
                    )
                    .role(ButtonRole::Destructive)
                    .control_size(ControlSize::Small)
                    .with_enabled(permissions.manage)
                    .on_effect(move || {
                        confirm.confirm(Confirm::Revoke {
                            grant: grant.clone(),
                            holder: holder.clone(),
                        });
                    }),
                )),
        )),
    )
}

/// The tab as the workspace's owner sees it, for captures.
pub(super) fn sample() -> Permissions {
    let choice = |key: &str, label: &str| Choice {
        key: key.to_owned(),
        label: label.to_owned(),
    };
    let permission = |key: &str, holder: &str, detail: &str, expiry: &str| Permission {
        key: key.to_owned(),
        holder: holder.to_owned(),
        detail: detail.to_owned(),
        expiry: expiry.to_owned(),
    };
    Permissions {
        intro: Intro {
            title: "Permissions".to_owned(),
            help: "What people, agents and apps may do here beyond their roles.".to_owned(),
            authority: "2 active".to_owned(),
        },
        saved_roles: "Saved roles (1)".to_owned(),
        manage: true,
        composer: Composer {
            header: "Give a permission".to_owned(),
            who: "Who".to_owned(),
            who_agent: "Which agent".to_owned(),
            subjects: vec![
                choice("member:ada", "Ada Lovelace-Byron"),
                choice("agent:claude", "claude"),
                choice("app:so.tilo.notes", "Notes"),
            ],
            nobody: "There is no one to give a permission to yet.".to_owned(),
            what: "What it allows".to_owned(),
            permissions: vec![
                choice("camera", "Use the camera"),
                choice("microphone", "Use the microphone"),
                choice("edit-files", "Edit the workspace's files"),
            ],
            how_long: "For how long".to_owned(),
            expiries: vec![
                choice("never", "Until revoked"),
                choice("session", "This session"),
                choice("once", "Once"),
            ],
            default_expiry: "session".to_owned(),
            grant: "Grant".to_owned(),
        },
        sections: vec![
            Holders {
                title: "Apps".to_owned(),
                permissions: vec![permission(
                    "cap-1",
                    "Notes",
                    "Use the camera, given by you",
                    "This session",
                )],
            },
            Holders {
                title: "People".to_owned(),
                permissions: vec![permission(
                    "grant-2",
                    "Ada Lovelace-Byron",
                    "Create roles, given by you",
                    "Until revoked",
                )],
            },
        ],
        revoke: "Revoke".to_owned(),
        empty: (
            "Nothing given yet".to_owned(),
            "Permissions you give show here.".to_owned(),
        ),
        footnote: None,
        revoke_decision: super::Decision {
            title: "Revoke this permission?".to_owned(),
            description: "It stops working at once, on every device.".to_owned(),
            keep: "Keep permission".to_owned(),
            review: "Review activity".to_owned(),
            confirm: "Revoke permission".to_owned(),
        },
    }
}
