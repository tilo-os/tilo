//! The Privacy & Security tab: what the workspace lets its people, agents
//! and apps do beyond their roles, grouped by who holds it, each with its
//! Take back, and a composer giving one more: who, what it allows, and for how
//! long.
//!
//! The composer's choices are the sheet's own until the person grants; the
//! grant names all three, so whoever asks for it says exactly what it gives.

use std::collections::BTreeMap;

use ui::format::{Date, Time};
use ui::icons::Icon;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, ControlSize, DateField, Dropdown, Element, EmptyState, ListRow,
    Row, Section, Segment, Text, TimeField, ToggleGroup,
};

use super::agents::{Intro, column, intro};
use super::general::{buttons, caption};
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
    /// How long it may last, by [`tilo_action_policy::Lifetime::key`]: `always`, and `until`, which
    /// asks for the day and time it ends. The first is what a new permission starts with.
    pub expiries: Vec<Choice>,
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

/// The documents and recipients for setting document access rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentAccess {
    pub title: String,
    pub documents: Vec<((String, String), String)>,
    pub rules: BTreeMap<(String, String), BTreeMap<String, String>>,
    pub recipients: Vec<Choice>,
    pub levels: Vec<Choice>,
    pub document_label: String,
    pub access_label: String,
    pub apply: String,
    pub empty: String,
}

/// Everything the tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    pub intro: Intro,
    /// Whether the person may give and revoke permissions.
    pub manage: bool,
    pub composer: Composer,
    pub documents: Option<DocumentAccess>,
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
    pub document: Option<(String, String)>,
    pub document_recipient: Option<String>,
    pub document_access: Option<String>,
    /// The day and time an `until` permission ends, as the person picked them.
    pub until: (Option<Date>, Option<Time>),
}

impl Draft {
    /// What the draft names among what `composer` offers now: a choice that
    /// went away falls back to the first one there is, the one the composer
    /// starts on.
    fn resolved(&self, composer: &Composer) -> (Option<Choice>, Option<Choice>, usize) {
        let pick = |chosen: &Option<String>, choices: &[Choice]| {
            chosen
                .as_ref()
                .and_then(|key| choices.iter().find(|choice| &choice.key == key))
                .or_else(|| choices.first())
                .cloned()
        };
        let expiry = self
            .expiry
            .as_ref()
            .and_then(|key| {
                composer
                    .expiries
                    .iter()
                    .position(|choice| &choice.key == key)
            })
            .unwrap_or_default();
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
    let mut groups: Vec<Element> = Vec::new();
    if !permissions.intro.title.is_empty() {
        groups.extend([
            intro(&permissions.intro, Icon::Shield),
            composer(permissions, parts),
        ]);
    }
    if let Some(documents) = &permissions.documents {
        groups.push(document_access(documents, &permissions.composer.who, parts));
    }
    if permissions.sections.is_empty() && !permissions.empty.0.is_empty() {
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

fn document_access(documents: &DocumentAccess, who: &str, parts: &Parts) -> Element {
    let draft = parts.local.get().draft;
    let Some((route, label)) = documents
        .documents
        .iter()
        .find(|(route, _)| Some(route) == draft.document.as_ref())
        .or_else(|| documents.documents.first())
    else {
        return Box::new(Section::titled(
            documents.title.clone(),
            vec![caption(&documents.empty)],
        ));
    };
    let Some(recipient) = choose(&documents.recipients, draft.document_recipient.as_deref()) else {
        return Box::new(Column::new().with_no_padding());
    };
    let saved = documents
        .rules
        .get(route)
        .and_then(|rules| rules.get(&recipient.key));
    let Some(access) = choose(
        &documents.levels,
        draft
            .document_access
            .as_deref()
            .or_else(|| saved.map(String::as_str)),
    ) else {
        return Box::new(Column::new().with_no_padding());
    };
    let (app, document) = route.clone();
    let effect = WorkspaceSettingsEffect::SetDocumentAccess {
        app,
        document,
        who: recipient.key.clone(),
        access: access.key.clone(),
    };
    let apply = parts.clone();
    Box::new(Section::titled(
        documents.title.clone(),
        vec![
            document_picker(documents, route, label, parts),
            Box::new(
                Row::new()
                    .with_rhythm(Rhythm::Related)
                    .align_start()
                    .push(picker(
                        "ws-document-access-who",
                        who,
                        recipient,
                        &documents.recipients,
                        true,
                        parts,
                        |draft, key| {
                            draft.document_recipient = Some(key);
                            draft.document_access = None;
                        },
                    ))
                    .push(picker(
                        "ws-document-access-level",
                        &documents.access_label,
                        access,
                        &documents.levels,
                        true,
                        parts,
                        |draft, key| draft.document_access = Some(key),
                    )),
            ),
            buttons(vec![
                Button::new("ws-document-access-apply", &documents.apply)
                    .role(ButtonRole::Primary)
                    .on_click(move || apply.ask(effect.clone())),
            ]),
        ],
    ))
}

/// Which document's access is shown, chosen by its place in the list: its route is two names.
fn document_picker(
    documents: &DocumentAccess,
    route: &(String, String),
    label: &str,
    parts: &Parts,
) -> Element {
    let choices: Vec<Choice> = documents
        .documents
        .iter()
        .enumerate()
        .map(|(at, (_, label))| Choice {
            key: at.to_string(),
            label: label.clone(),
        })
        .collect();
    let at = documents.documents.iter().position(|(key, _)| key == route);
    let shown = Choice {
        key: at.unwrap_or_default().to_string(),
        label: label.to_owned(),
    };
    let routes: Vec<(String, String)> = documents
        .documents
        .iter()
        .map(|(key, _)| key.clone())
        .collect();
    picker(
        "ws-document-access-document",
        &documents.document_label,
        &shown,
        &choices,
        true,
        parts,
        move |draft, at| {
            draft.document = at
                .parse::<usize>()
                .ok()
                .and_then(|at| routes.get(at))
                .cloned();
            draft.document_access = None;
        },
    )
}

fn choose<'a>(choices: &'a [Choice], key: Option<&str>) -> Option<&'a Choice> {
    choices
        .iter()
        .find(|choice| Some(choice.key.as_str()) == key)
        .or_else(|| choices.first())
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
    let chosen = composer.expiries.get(expiry);
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
            expiry,
        )
        .with_id("ws-grant-expiry")
        .with_accessible_name(composer.how_long.clone())
        .with_enabled(permissions.manage)
        .on_choose("ws-grant-expiry", move |index| {
            if let Some(key) = keys.get(index) {
                let key = key.clone();
                choose.local.update(|local| local.draft.expiry = Some(key));
            }
        }),
    ));
    let until = chosen.is_some_and(|choice| choice.key == UNTIL).then(|| {
        let (day, time) = parts.local.get().draft.until;
        items.push(until_fields(day, time, parts));
        ends_at(day, time)
    });
    let grant = parts.clone();
    let named = subject
        .as_ref()
        .zip(permission.as_ref())
        .map(|(subject, permission)| (subject.key.clone(), permission.key.clone()));
    // An `until` permission is given once it names a day and time still ahead.
    let can_grant = permissions.manage && named.is_some() && until.is_none_or(|at| at.is_some());
    items.push(buttons(vec![
        Button::new("ws-grant", &composer.grant)
            .role(ButtonRole::Primary)
            .with_enabled(can_grant)
            .on_click(move || {
                if let Some((subject, permission)) = named.clone() {
                    grant.ask(WorkspaceSettingsEffect::Grant {
                        subject,
                        permission,
                        until: until.flatten(),
                    });
                }
            }),
    ]));
    Box::new(Section::titled(composer.header.clone(), items))
}

/// The expiry that asks for the day and time a permission ends.
const UNTIL: &str = "until";

/// The day an `until` permission ends, picked on the toolkit's date field, and the time of that
/// day in its popover.
fn until_fields(day: Option<Date>, time: Option<Time>, parts: &Parts) -> Element {
    use ui::component::{Bound, Described as _};
    let (on_day, on_time) = (parts.clone(), parts.clone());
    let props = ui::widget::DateFieldProps {
        value: Bound {
            state: ui::LocalState::new(day.map(|day| day.to_string()).unwrap_or_default()),
            ..Bound::default()
        },
        at: time.map(|time| time.strftime("%H:%M").to_string()),
        time: vec![Box::new(
            TimeField::new("ws-grant-until-time", time)
                .on_change(move |time| on_time.local.update(|local| local.draft.until.1 = time)),
        )],
        ..ui::widget::DateFieldProps::default()
    };
    Box::new(
        DateField::from_props("ws-grant-until-day", props)
            .on_change(move |day| on_day.local.update(|local| local.draft.until.0 = day)),
    )
}

/// The instant a permission picked to end on `day` at `time` ends, in Unix milliseconds: `None`
/// until both are picked, and for one already past.
fn ends_at(day: Option<Date>, time: Option<Time>) -> Option<u64> {
    let at = ui::clock::instant_of(day?.to_datetime(time?))?;
    (at > ui::clock::local_now().timestamp())
        .then(|| u64::try_from(at.as_millisecond()).ok())
        .flatten()
}

/// A chooser showing what is chosen, under its caption. What it chooses is the composer's draft,
/// the sheet's own until the person grants: the chooser's own change, which a script and an agent
/// make as a person does, moves it.
fn picker(
    id: &str,
    caption_text: &str,
    chosen: &Choice,
    choices: &[Choice],
    enabled: bool,
    parts: &Parts,
    set: impl Fn(&mut Draft, String) + 'static,
) -> Element {
    let choose = parts.clone();
    let mut chooser = Dropdown::from_state(
        choices.iter().map(|choice| choice.label.clone()).collect(),
        &ui::LocalState::new(chosen.key.clone()),
    )
    .with_id(id)
    .with_accessible_name(caption_text)
    .with_enabled(enabled)
    .on_change(move |key| {
        let key = key.to_owned();
        choose.local.update(|local| set(&mut local.draft, key));
    });
    chooser.set_value_options(
        choices
            .iter()
            .map(|choice| (choice.key.clone(), choice.label.clone()))
            .collect(),
    );
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(caption(caption_text))
            .push(Box::new(chooser)),
    )
}

/// One permission: who holds it, what it allows and who gave it, how long
/// it lasts, and Take back, which asks first.
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
                    .on_click(move || {
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
                choice("network", "Network access"),
                choice("read-files", "Read the workspace's files"),
                choice("edit-files", "Edit the workspace's files"),
            ],
            how_long: "For how long".to_owned(),
            expiries: vec![choice("always", "Always"), choice("until", "Until")],
            grant: "Grant".to_owned(),
        },
        documents: None,
        sections: vec![
            Holders {
                title: "Apps".to_owned(),
                permissions: vec![permission(
                    "cap-1",
                    "Notes",
                    "Use the camera, given by you",
                    "Until Tomorrow 5:30 PM",
                )],
            },
            Holders {
                title: "People".to_owned(),
                permissions: vec![permission(
                    "grant-2",
                    "Ada Lovelace-Byron",
                    "Create roles, given by you",
                    "Always",
                )],
            },
        ],
        revoke: "Take back".to_owned(),
        empty: (
            "Nothing given yet".to_owned(),
            "Permissions you give show here.".to_owned(),
        ),
        footnote: None,
        revoke_decision: super::Decision {
            title: "Take back this permission?".to_owned(),
            description: "It stops working at once, on every device.".to_owned(),
            keep: "Keep permission".to_owned(),
            review: "Review activity".to_owned(),
            confirm: "Take back permission".to_owned(),
        },
    }
}
