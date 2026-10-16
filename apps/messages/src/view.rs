use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_contact_directory::{ContactDirectory, routing};
use tilo_messaging::PersonAccounts;
use ui::LocalState;
use ui::framework::{Actions, Effected, Fixture, PersonRecord, ViewCx};
use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::prelude::{Collection, Id, Json};
use ui::widget::virtual_list::{VecListModel, VirtualList};
use ui::widget::{
    Button, Column, Container, Dropdown, Element, ListRow, ModelView, NavPane, NavShell, Row,
    Scrollable, Text, TextInput,
};

use crate::{CATALOG, Messages, MessagesEffect};

#[derive(Clone, Default)]
struct CapturePeople {
    directory: ContactDirectory,
    accounts: PersonAccounts,
}

#[derive(Clone)]
pub(crate) struct Sources {
    directory: Option<PersonRecord<ContactDirectory>>,
    accounts: Option<PersonRecord<PersonAccounts>>,
    fixture: Rc<CapturePeople>,
}

impl Sources {
    pub(crate) fn contacts(&self) -> Rc<ContactDirectory> {
        self.directory.as_ref().map_or_else(
            || Rc::new(self.fixture.directory.clone()),
            PersonRecord::get,
        )
    }
    pub(crate) fn accounts(&self) -> Rc<PersonAccounts> {
        self.accounts
            .as_ref()
            .map_or_else(|| Rc::new(self.fixture.accounts.clone()), PersonRecord::get)
    }
    fn revision(&self) -> (u64, u64) {
        (
            self.directory.as_ref().map_or(0, PersonRecord::revision),
            self.accounts.as_ref().map_or(0, PersonRecord::revision),
        )
    }
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct Selection {
    pub(crate) person: String,
    pub(crate) choice: String,
}

/// Values are exact identities; localized labels never round-trip as keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Choice {
    pub(crate) service: String,
    pub(crate) account: String,
    pub(crate) recipient: String,
    pub(crate) association: String,
    pub(crate) scope: String,
    pub(crate) revision: String,
}

impl Choice {
    pub(crate) fn value(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub(crate) fn label(&self, sources: &Sources) -> String {
        let account = if self.account.is_empty() {
            CATALOG.t("messages-no-account")
        } else if self.service == "tilo" {
            CATALOG.t("messages-native-account")
        } else {
            sources
                .accounts()
                .accounts
                .get(&Id::from_raw(&self.account))
                .map_or_else(
                    || self.account.clone(),
                    |account| format!("{} ({})", account.display_name, self.account),
                )
        };
        let service = CATALOG.t(&format!("messages-{}", self.service));
        if self.recipient.is_empty() {
            CATALOG.t_args(
                "messages-account-label",
                &[
                    ("service", ArgValue::Text(service)),
                    ("account", ArgValue::Text(account)),
                ],
            )
        } else {
            CATALOG.t_args(
                "messages-route-label",
                &[
                    ("service", ArgValue::Text(service)),
                    ("account", ArgValue::Text(account)),
                    ("recipient", ArgValue::Text(self.recipient.clone())),
                ],
            )
        }
    }
}

pub(crate) fn choices(sources: &Sources, person: &str) -> Vec<Choice> {
    let contacts = sources.contacts();
    let person = Id::from_raw(person);
    if contacts
        .people
        .get(&person)
        .is_none_or(|person| person.removed)
    {
        return Vec::new();
    }
    let accounts = sources.accounts();
    let mut result = Vec::new();
    for service in ["tilo", "whatsapp", "signal", "sms"] {
        let senders: Vec<_> = accounts
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && account.type_id == service)
            .map(|(id, account)| routing::SenderAccount {
                id: id.clone(),
                service: service.into(),
                enabled: account.enabled,
                scope: account
                    .config
                    .0
                    .get("messaging_scope")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .into(),
            })
            .collect();
        let routes = routing::routes(&contacts, &person, service, &senders);
        if routes.is_empty() {
            // Metadata can describe an unavailable account. It never supplies
            // a recipient or identity revision to bypass the shared resolver.
            let ids: Vec<_> = if senders.is_empty() {
                vec![String::new()]
            } else {
                senders
                    .iter()
                    .map(|sender| sender.id.as_str().to_owned())
                    .collect()
            };
            for account in ids {
                result.push(Choice {
                    service: service.into(),
                    account,
                    recipient: String::new(),
                    association: String::new(),
                    scope: String::new(),
                    revision: String::new(),
                });
            }
        } else {
            result.extend(routes.into_iter().map(|route| Choice {
                service: route.service,
                account: route.sender.as_str().into(),
                recipient: route.recipient,
                association: route.association.as_str().into(),
                scope: route.scope,
                revision: route.revision,
            }));
        }
    }
    result
}

fn answer(
    sources: &Sources,
    state: &LocalState<Selection>,
    effect: MessagesEffect,
) -> ui::framework::EffectAnswer {
    match effect {
        MessagesEffect::ListPeople => {
            let people: Vec<_> = sources
                .contacts()
                .people
                .iter()
                .filter(|(_, person)| !person.removed)
                .map(|(id, person)| serde_json::json!({"person":id.as_str(), "label":person.label}))
                .collect();
            return Ok(Effected::value(people));
        }
        MessagesEffect::ListRoutes { person } => {
            return Ok(Effected::value(choices(sources, &person)));
        }
        MessagesEffect::OpenPerson { person } => {
            if sources
                .contacts()
                .people
                .get(&Id::from_raw(&person))
                .is_none_or(|person| person.removed)
            {
                return Err(CATALOG.t("messages-contact-missing"));
            }
            if state.get().person != person {
                state.set(Selection {
                    person,
                    choice: String::new(),
                });
            }
        }
        MessagesEffect::SelectRoute { choice } => {
            let selected = state.get();
            let requested: Choice = serde_json::from_str(&choice)
                .map_err(|_| CATALOG.t("messages-association-conflict"))?;
            let Some(valid) = choices(sources, &selected.person)
                .into_iter()
                .find(|candidate| candidate == &requested)
            else {
                return Err(CATALOG.t("messages-contact-missing"));
            };
            if !valid.recipient.is_empty()
                && sources.contacts().resolve_account(
                    &valid.service,
                    &valid.scope,
                    &valid.recipient,
                    &valid.revision,
                ) != Ok(Id::from_raw(&selected.person))
            {
                return Err(CATALOG.t("messages-association-conflict"));
            }
            state.set(Selection {
                choice: valid.value(),
                ..selected
            });
        }
        MessagesEffect::Back => state.set(Selection::default()),
        MessagesEffect::ConfirmAccount { person, choice } => {
            confirm_account(sources, state, person, &choice)?;
        }

        MessagesEffect::ResetFirstEnrollment
        | MessagesEffect::OpenAccountSetup
        | MessagesEffect::SelectSetupAccount { .. }
        | MessagesEffect::BeginFirstEnrollment { .. }
        | MessagesEffect::CompleteFirstEnrollment { .. }
        | MessagesEffect::ReviewFirstEnrollment { .. }
        | MessagesEffect::ListSmsSims
        | MessagesEffect::SelectSmsSim { .. }
        | MessagesEffect::BeginLink { .. }
        | MessagesEffect::CompleteLink { .. }
        | MessagesEffect::CancelLink
        | MessagesEffect::ListAccountStates
        | MessagesEffect::ListGroups { .. }
        | MessagesEffect::ReadGroupMembership { .. }
        | MessagesEffect::ReadGroupHistory { .. }
        | MessagesEffect::SendGroupText { .. }
        | MessagesEffect::AddGroupMember { .. }
        | MessagesEffect::RemoveGroupMember { .. }
        | MessagesEffect::LeaveGroup { .. }
        | MessagesEffect::CloseGroups
        | MessagesEffect::ReadHistory { .. }
        | MessagesEffect::Send { .. } => {
            return Err(CATALOG.t("messages-transport-unavailable"));
        }
    }
    Ok(Effected::done())
}

fn confirm_account(
    sources: &Sources,
    state: &LocalState<Selection>,
    person: String,
    choice: &str,
) -> Result<(), String> {
    let choice: Choice =
        serde_json::from_str(choice).map_err(|_| CATALOG.t("messages-association-conflict"))?;
    if choice.account.is_empty()
        || choice.association.is_empty()
        || choice.scope.is_empty()
        || choice.revision.is_empty()
    {
        return Err(CATALOG.t("messages-identity-unavailable"));
    }
    let accounts = sources.accounts();
    let valid_sender = accounts
        .accounts
        .get(&Id::from_raw(&choice.account))
        .is_some_and(|account| {
            account.enabled
                && account.type_id == choice.service
                && account
                    .config
                    .0
                    .get("messaging_scope")
                    .and_then(serde_json::Value::as_str)
                    == Some(choice.scope.as_str())
        });
    if !valid_sender {
        return Err(CATALOG.t("messages-no-account"));
    }
    let directory = sources
        .directory
        .as_ref()
        .ok_or_else(|| CATALOG.t("messages-contact-missing"))?;
    directory
        .invoke(&ContactDirectory::CONFIRM_ACCOUNT.with((
            Id::from_raw(&person),
            Id::from_raw(&choice.association),
            choice.service.clone(),
            choice.scope.clone(),
            choice.recipient.clone(),
            choice.revision.clone(),
        )))
        .map_err(|error| error.to_string())?;
    if state.get().person == person {
        state.set(Selection {
            person,
            choice: choice.value(),
        });
    }
    Ok(())
}

fn sources(cx: &ViewCx<Messages>) -> Sources {
    Sources {
        directory: if cx.mode().is_live() && !cx.is_past() {
            cx.shared_person::<ContactDirectory>()
        } else {
            None
        },
        accounts: if cx.mode().is_live() && !cx.is_past() {
            cx.shared_person::<PersonAccounts>()
        } else {
            None
        },
        fixture: cx.fixture::<CapturePeople>().unwrap_or_default(),
    }
}

pub(super) fn build(cx: &ViewCx<Messages>) -> Element {
    let sources = sources(cx);
    let selection = LocalState::new(Selection::default());
    let live = cx.mode().is_live() && !cx.is_past();
    let sending = crate::sending::Sending::new(live.then(|| cx.take_lent()).flatten(), cx.me());
    let history = crate::history::HistoryView::new(
        sending
            .history()
            .or_else(|| live.then(|| cx.take_lent()).flatten()),
        cx.me(),
    );
    if !cx.mode().is_live()
        && let Some(capture) = cx.fixture::<crate::history::CaptureHistory>()
    {
        history.stage_capture(capture.0.clone());
    }
    let setup = crate::setup::SetupView::new(
        sending.client(),
        live.then(|| cx.take_lent()).flatten(),
        cx.me(),
    );
    let groups = crate::groups::GroupsView::new(sending.client());
    register_effects(
        cx, &sources, &selection, &sending, &history, &setup, &groups,
    );
    register_capture_selection(cx, &sources, &selection);
    let changed_history = history.clone();
    let watched_history = history.clone();
    let watched_groups = groups.clone();
    let watched_sources = sources.clone();
    let watched_selection = selection.clone();
    let watched_sending = sending.clone();
    let watched_jobs = cx.jobs();
    let last_change = std::cell::Cell::new(history.revision());
    let projected = sources.clone();
    let projected_selection = selection.clone();
    let actions = cx.actions();
    let projected_setup = setup.clone();
    let scheduled_setup = setup.clone();
    let projected_groups = groups.clone();
    Box::new(
        ModelView::from_document(
            move |_, _| {
                (
                    projected.revision(),
                    projected_selection.get(),
                    changed_history.revision(),
                    projected_setup.key(),
                    projected_groups.key(),
                )
            },
            move |(_, selected, change, _, _)| {
                if discard_invalid_route(
                    &watched_sources,
                    &watched_selection,
                    &watched_sending,
                    &watched_history,
                ) {
                    watched_jobs.cancel("messages.history");
                }
                if last_change.replace(*change) != *change && watched_history.available() {
                    watched_groups.refresh_loaded(&watched_sources, &watched_jobs);
                    let choice = watched_selection.get().choice;
                    watched_jobs.start::<()>("messages.history", |cx| {
                        watched_history.read(&watched_sources, &watched_selection, &choice, cx)
                    });
                }
                let detail_sources = sources.clone();
                let detail_actions = actions.clone();
                let detail_history = history.clone();
                let detail_sending = sending.clone();
                let conversation = Box::new(ModelView::from_state(&selection, move |selected| {
                    detail(
                        &detail_sources,
                        &detail_actions,
                        selected,
                        &detail_history,
                        &detail_sending,
                    )
                }));
                let detail = if groups.visible() {
                    groups.panel(&sources, &actions)
                } else if setup.visible() {
                    setup.panel(&sources, &actions)
                } else {
                    conversation
                };
                let detail = Box::new(
                    Container::new()
                        .with_no_padding()
                        .filling()
                        .with_child(detail),
                );
                navigation(detail, &sources, &actions, selected, &setup, &groups)
            },
        )
        .with_wake_deadline(move || scheduled_setup.next_wake_deadline()),
    )
}

fn navigation(
    detail: Element,
    sources: &Sources,
    actions: &Actions<Messages>,
    selected: &Selection,
    setup: &crate::setup::SetupView,
    groups: &crate::groups::GroupsView,
) -> Element {
    let overview = Box::new(
        Column::new()
            .push(groups.entries(sources, actions))
            .push(overview(sources, actions, &selected.person)),
    );
    let pane = LocalState::new(
        if selected.person.is_empty() && !setup.visible() && !groups.visible() {
            NavPane::Overview
        } else {
            NavPane::Content
        },
    );
    Box::new(
        NavShell::new()
            .overview(overview)
            .overview_header(
                CATALOG.t("messages-people"),
                vec![account_setup_action(actions)],
            )
            .content(detail)
            .content_header_leading(if groups.visible() {
                Box::new(Text::new(&CATALOG.t("messages-list-groups")))
            } else if setup.visible() {
                Box::new(Text::new(&CATALOG.t("messages-setup-accounts")))
            } else {
                route_header(sources, actions, selected)
            })
            .with_pane_state(&pane),
    )
}

fn discard_invalid_route(
    sources: &Sources,
    selection: &LocalState<Selection>,
    sending: &crate::sending::Sending,
    history: &crate::history::HistoryView,
) -> bool {
    // Shared account/contact edits can invalidate a selection without
    // going through this app's navigation effects.
    let selected = selection.get();
    if selected.choice.is_empty()
        || choices(sources, &selected.person)
            .iter()
            .any(|choice| choice.value() == selected.choice)
    {
        return false;
    }
    sending.clear();
    history.clear();
    selection.set(Selection {
        person: selected.person,
        choice: String::new(),
    });
    true
}

fn account_setup_action(actions: &Actions<Messages>) -> Element {
    Box::new(actions.bind_effect_invocation(
        Button::new("messages.accounts", "messages-setup-accounts"),
        &MessagesEffect::OpenAccountSetup,
    ))
}

fn register_capture_selection(
    cx: &ViewCx<Messages>,
    sources: &Sources,
    selection: &LocalState<Selection>,
) {
    let captured_selection = selection.clone();
    let stage_sources = sources.clone();
    cx.on_stage(move |scenario| {
        if scenario.is_some_and(|name| name.starts_with("person") || name.starts_with("history")) {
            let person = "00000000-0000-4000-8000-000000000001".to_owned();
            let choice = choices(&stage_sources, &person)
                .into_iter()
                .find(|choice| choice.service == "signal")
                .map_or_else(String::new, |choice| choice.value());
            captured_selection.set(Selection { person, choice });
        }
    });
}

fn register_effects(
    cx: &ViewCx<Messages>,
    sources: &Sources,
    selection: &LocalState<Selection>,
    sending: &crate::sending::Sending,
    history: &crate::history::HistoryView,
    setup: &crate::setup::SetupView,
    groups: &crate::groups::GroupsView,
) {
    let effect_groups = groups.clone();
    let effect_setup = setup.clone();
    let effect_sending = sending.clone();
    let state = selection.clone();
    let effect_sources = sources.clone();
    let effect_history = history.clone();
    let status_sending = sending.clone();
    let jobs = cx.jobs();
    let status_history = history.clone();
    cx.effects(move |effect: MessagesEffect, cx| {
        if crate::groups::GroupsView::handles(&effect) { effect_setup.close(); return effect_groups.effect(&effect_sources, effect, cx); }
        if crate::setup::SetupView::handles(&effect) { effect_groups.close(); return effect_setup.effect(&effect_sources, effect, cx); }
        if matches!(effect, MessagesEffect::OpenPerson { .. } | MessagesEffect::Back) { effect_setup.close(); effect_groups.close(); }

        if matches!(effect, MessagesEffect::ListAccountStates) {
            let values: Vec<_> = effect_sources.accounts().accounts.iter().filter(|(_, account)| matches!(account.type_id.as_str(), "tilo" | "whatsapp" | "signal" | "sms")).map(|(id, account)| {
                let choice = Choice {service: account.type_id.clone(), account: id.as_str().into(), recipient: String::new(), association: String::new(), scope: String::new(), revision: String::new()};
                serde_json::json!({"service": choice.service, "account": choice.account, "enabled": account.enabled, "state": effect_sending.account_state(&choice).as_str()})
            }).collect();
            return Ok(Effected::value(values));
        }
        if matches!(effect, MessagesEffect::Send { .. }) {
            return effect_sending.send(&effect_sources, &state, &effect_history, effect, cx);
        }
        if let MessagesEffect::ReadHistory { choice } = effect {
            return effect_history.read(&effect_sources, &state, &choice, cx);
        }
        let before = state.get();
        let result = answer(&effect_sources, &state, effect)?;
        let selected = state.get();
        let changed = selected != before;
        if changed {
            effect_sending.clear();
            effect_history.clear();
            jobs.cancel("messages.history");
        }
        if changed && !selected.choice.is_empty() && effect_history.available() {
            let choice = selected.choice;
            jobs.start::<()>("messages.history", |cx| {
                effect_history.read(&effect_sources, &state, &choice, cx)
            });
        }
        Ok(result)
    })
    .status(move || {
        let mut status = status_history.status();
        status.extend(status_sending.status());
        status
    });
}

fn overview(sources: &Sources, actions: &Actions<Messages>, selected: &str) -> Element {
    let people: Vec<_> = sources
        .contacts()
        .people
        .iter()
        .filter(|(_, person)| !person.removed)
        .map(|(id, person)| (id.clone(), person.label.clone()))
        .collect();
    if people.is_empty() {
        return empty("messages-empty-heading", "messages-empty-hint");
    }
    let Ok(model) = VecListModel::new(people, |(id, _)| id.clone()) else {
        return empty("messages-contact-missing", "messages-empty-hint");
    };
    let controls = actions.clone();
    let selected = selected.to_owned();
    let list = VirtualList::new(model, 64.0, move |model, index, id| {
        let label = model
            .item(index)
            .map_or_else(String::new, |(_, label)| label.clone());
        let open = MessagesEffect::OpenPerson {
            person: id.as_str().into(),
        };
        let row = ListRow::new(format!("messages.person:{id}"), &label)
            .avatar(&label, id.as_str())
            .selected(id.as_str() == selected);
        Box::new(match controls.effect(&open) {
            Some(binding) => row.on_action(binding),
            None => row.with_enabled(false),
        })
    })
    .with_id("messages.people")
    .with_accessible_name(CATALOG.t("messages-people"))
    .with_semantic_key(ToString::to_string);
    Box::new(Container::new().filling().with_child(Box::new(list)))
}

fn route_header(sources: &Sources, actions: &Actions<Messages>, selected: &Selection) -> Element {
    let options = choices(sources, &selected.person);
    let accessible = options
        .iter()
        .find(|choice| choice.value() == selected.choice)
        .map_or_else(
            || CATALOG.t("messages-route"),
            |choice| {
                CATALOG.t_args(
                    "messages-route-current",
                    &[("route", ArgValue::Text(choice.label(sources)))],
                )
            },
        );
    let value = LocalState::new(selected.choice.clone());
    let mut dropdown = Dropdown::from_state(Vec::new(), &value)
        .with_id("messages.route")
        .with_accessible_name(accessible)
        .with_placeholder(&CATALOG.t("messages-route-placeholder"))
        .as_menu(Icon::ChevronDown);
    dropdown.set_value_options(
        options
            .iter()
            .map(|choice| (choice.value(), choice.label(sources)))
            .collect(),
    );
    let dropdown = actions.bind_effect(dropdown, MessagesEffect::SELECT_ROUTE);
    let name = sources
        .contacts()
        .people
        .get(&Id::from_raw(&selected.person))
        .map_or_else(String::new, |person| person.label.clone());
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(Text::dynamic(move || name.clone())))
            .push(Box::new(dropdown)),
    )
}

fn detail(
    sources: &Sources,
    actions: &Actions<Messages>,
    selected: &Selection,
    history_view: &crate::history::HistoryView,
    sending: &crate::sending::Sending,
) -> Element {
    if selected.person.is_empty() {
        return empty("messages-choose-heading", "messages-choose-hint");
    }
    let contacts = sources.contacts();
    let Some(_person) = contacts
        .people
        .get(&Id::from_raw(&selected.person))
        .filter(|person| !person.removed)
    else {
        return empty("messages-contact-missing", "messages-choose-hint");
    };
    let options = choices(sources, &selected.person);
    let chosen = options
        .iter()
        .find(|choice| choice.value() == selected.choice);
    let hint = match chosen {
        None => "messages-not-selected",
        Some(choice) if choice.recipient.is_empty() => "messages-no-recipient",
        Some(choice) if choice.account.is_empty() => "messages-no-account",
        Some(_) => "messages-history-unavailable",
    };
    let history = Scrollable::new_vertical()
        .fill_parent_height()
        .with_semantic_key(format!(
            "messages.history:{}:{}",
            selected.person, selected.choice
        ))
        .with_child(history_view.widget(sources, actions, selected, chosen, hint));
    let mut body = Column::new();
    if let Some(choice) = chosen {
        let label = choice.label(sources);
        body = body.push(Box::new(Text::dynamic(move || label.clone())));
        {
            let key = if choice.account.is_empty() {
                "messages-account-unconfigured".into()
            } else {
                format!(
                    "messages-account-{}",
                    sending.account_state(choice).as_str()
                )
            };
            body = body.push(Box::new(Text::new(&key).with_id("messages.account-state")));
        }
    }
    body = body.push(Box::new(
        Column::new()
            .with_no_padding()
            .with_fill_remaining_height()
            .push(Box::new(history)),
    ));
    if chosen.is_some_and(|choice| choice.recipient.is_empty()) {
        body = body.push(Box::new(Text::new("messages-identity-unavailable")));
    }
    Box::new(body.push(composer(sources, actions, sending, selected, chosen)))
}

fn composer(
    sources: &Sources,
    actions: &Actions<Messages>,
    sending: &crate::sending::Sending,
    selection: &Selection,
    choice: Option<&Choice>,
) -> Element {
    let enabled = choice
        .and_then(|c| crate::history::current_route(sources, selection, c).ok())
        .is_some_and(|route| sending.supports(&route));
    let draft = sending.draft.clone();
    let text = draft.clone();
    let operation = sending.operation.clone();
    let choice = selection.choice.clone();
    let send = actions.bind_effect(
        Button::new("messages.send", "messages-send").with_enabled(enabled),
        MessagesEffect::SEND.map(move |()| (choice.clone(), operation.get(), text.get())),
    );
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(
                        TextInput::from_state(&draft)
                            .with_id("messages.composer")
                            .with_accessible_name(CATALOG.t("messages-compose"))
                            .with_placeholder(CATALOG.t("messages-compose"))
                            .disabled(!enabled),
                    )),
            ))
            .push(Box::new(send)),
    )
}

pub(crate) fn empty(title: &str, hint: &str) -> Element {
    Box::new(
        Container::new().with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(Text::new(title).with_heading_level(2)))
                .push(Box::new(Text::new(hint))),
        )),
    )
}

pub(super) fn fixture(fixture: &mut Fixture<Messages>) {
    if fixture
        .scenario()
        .is_none_or(|name| !(name.starts_with("person") || name.starts_with("history")))
    {
        return;
    }
    let mut directory = ContactDirectory::default();
    let person = Id::from_raw("00000000-0000-4000-8000-000000000001");
    let _ = directory.project_contact(person.clone(), "Ada Rivera".into());
    let _ = directory.confirm_account(
        person.clone(),
        Id::from_raw("signal"),
        "signal".into(),
        "fixture-signal-scope".into(),
        "+12025550123".into(),
        "fixture-signal-revision".into(),
    );
    let _ = directory.confirm_account(
        person,
        Id::from_raw("whatsapp"),
        "whatsapp".into(),
        "fixture-whatsapp-scope".into(),
        "12025550123@s.whatsapp.net".into(),
        "fixture-whatsapp-revision".into(),
    );
    let mut accounts = PersonAccounts {
        accounts: Collection::new(),
        ..PersonAccounts::default()
    };
    for (id, service, label) in [
        ("fixture-signal", "signal", "Personal"),
        ("fixture-whatsapp", "whatsapp", "Work"),
    ] {
        accounts.accounts.push(
            Id::from_raw(id),
            tilo_messaging::PersonAccount {
                type_id: service.into(),
                display_name: label.into(),
                config: Json(serde_json::json!({"messaging_scope":format!("{id}-scope")})),
                enabled: true,
                tint: 0,
            },
        );
    }
    if fixture
        .scenario()
        .is_some_and(|name| name.starts_with("history"))
    {
        fixture.provide(crate::history::CaptureHistory(capture_history()));
    }
    fixture.provide(CapturePeople {
        directory,
        accounts,
    });
}

fn capture_history() -> tilo_messaging::HistoryPage {
    use tilo_messaging::{Delivery, Direction, HistoryPage, Message};
    HistoryPage {
        route: routing::Route {
            person: Id::from_raw("00000000-0000-4000-8000-000000000001"),
            association: Id::from_raw("signal"),
            sender: Id::from_raw("fixture-signal"),
            service: "signal".into(),
            scope: "fixture-signal-scope".into(),
            recipient: "+12025550123".into(),
            revision: "fixture-signal-revision".into(),
        },
        more: false,
        messages: [
            ("capture-1", "Are you nearby?", Direction::Incoming, None),
            (
                "capture-2",
                "On my way. See you in ten minutes!",
                Direction::Outgoing,
                Some(Delivery::RecipientDelivered),
            ),
            (
                "capture-3",
                "Perfect. I'll find us a table by the window.",
                Direction::Incoming,
                None,
            ),
        ]
        .into_iter()
        .map(|(id, text, direction, delivery)| Message {
            id: id.into(),
            text: text.to_owned().into(),
            direction,
            timestamp_ms: 1,
            delivery,
        })
        .collect(),
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::MessagesShell;
    use ui::framework::testing::TestApp;

    const PERSON: &str = "00000000-0000-4000-8000-000000000001";

    fn choose(
        sources: &Sources,
        state: &LocalState<Selection>,
        choice: &Choice,
    ) -> ui::framework::EffectAnswer {
        answer(
            sources,
            state,
            MessagesEffect::SelectRoute {
                choice: choice.value(),
            },
        )
    }
    fn review(
        sources: &Sources,
        state: &LocalState<Selection>,
        person: &str,
        choice: &Choice,
    ) -> ui::framework::EffectAnswer {
        answer(
            sources,
            state,
            MessagesEffect::ConfirmAccount {
                person: person.into(),
                choice: choice.value(),
            },
        )
    }
    #[test]
    fn selected_route_is_retired_by_revocation_identity_revision_and_account_disconnect() {
        let mut app = TestApp::<MessagesShell>::for_person("route-fixture").expect("headless app");
        let directory = app
            .shared_person::<ContactDirectory>()
            .expect("shared directory");
        directory
            .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(PERSON), "Alex".into())))
            .expect("person");
        let mut accounts = PersonAccounts::default();
        accounts
            .save_account(
                "sender".into(),
                "signal".into(),
                "Personal".into(),
                serde_json::json!({"messaging_scope":"signal-fixture"}),
                true,
                0,
            )
            .expect("metadata fixture");
        let mut sources = Sources {
            directory: Some(directory.clone()),
            accounts: None,
            fixture: Rc::new(CapturePeople {
                directory: ContactDirectory::default(),
                accounts,
            }),
        };
        let selection = LocalState::new(Selection {
            person: PERSON.into(),
            choice: String::new(),
        });
        let old = Choice {
            service: "signal".into(),
            account: "sender".into(),
            recipient: "+12025550123".into(),
            association: "association-1".into(),
            scope: "signal-fixture".into(),
            revision: "identity-1".into(),
        };
        review(&sources, &selection, PERSON, &old).expect("reviewed association");
        choose(&sources, &selection, &old).expect("select exact route");
        assert_eq!(selection.get().choice, old.value());
        let changed = Choice {
            revision: "identity-2".into(),
            association: "association-2".into(),
            ..old.clone()
        };
        assert!(choose(&sources, &selection, &changed).is_err());
        assert!(
            review(&sources, &selection, PERSON, &changed).is_err(),
            "changed identity requires explicit revocation first"
        );
        directory
            .invoke(
                &ContactDirectory::FORGET_ENDPOINT
                    .with((Id::from_raw(PERSON), Id::from_raw(&old.association))),
            )
            .expect("revoke");
        assert!(choose(&sources, &selection, &old).is_err());
        assert!(
            review(&sources, &selection, PERSON, &old).is_err(),
            "tombstoned association must never revive"
        );
        review(&sources, &selection, PERSON, &changed)
            .expect("review new identity under fresh association");
        choose(&sources, &selection, &changed).expect("new exact route");
        assert!(choose(&sources, &selection, &old).is_err());
        let mut disconnected = sources.fixture.as_ref().clone();
        disconnected
            .accounts
            .accounts
            .get_mut(&Id::from_raw("sender"))
            .expect("sender")
            .enabled = false;
        sources.fixture = Rc::new(disconnected);
        assert!(
            choose(&sources, &selection, &changed).is_err(),
            "never fall back to another account"
        );
    }

    #[test]
    fn scoped_confirmation_cannot_substitute_another_person_sender_or_provider_namespace() {
        let mut app = TestApp::<MessagesShell>::for_person("scope-fixture").expect("headless app");
        let directory = app
            .shared_person::<ContactDirectory>()
            .expect("shared directory");
        for person in [PERSON, "00000000-0000-4000-8000-000000000002"] {
            directory
                .invoke(
                    &ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(person), "Alex".into())),
                )
                .expect("distinct same-name identity");
        }
        let mut accounts = PersonAccounts::default();
        accounts
            .save_account(
                "sender".into(),
                "signal".into(),
                "Personal".into(),
                serde_json::json!({"messaging_scope":"tenant-1"}),
                true,
                0,
            )
            .expect("metadata");
        let sources = Sources {
            directory: Some(directory),
            accounts: None,
            fixture: Rc::new(CapturePeople {
                directory: ContactDirectory::default(),
                accounts,
            }),
        };
        let selection = LocalState::new(Selection {
            person: PERSON.into(),
            choice: String::new(),
        });
        let choice = Choice {
            service: "signal".into(),
            account: "sender".into(),
            recipient: "+12025550123".into(),
            association: "reviewed-link".into(),
            scope: "tenant-1".into(),
            revision: "key-1".into(),
        };
        for forged in [
            Choice {
                account: "other-sender".into(),
                ..choice.clone()
            },
            Choice {
                scope: "tenant-2".into(),
                ..choice.clone()
            },
        ] {
            assert!(review(&sources, &selection, PERSON, &forged).is_err());
        }
        review(&sources, &selection, PERSON, &choice).expect("confirmed exact identity");
        assert!(
            answer(
                &sources,
                &selection,
                MessagesEffect::ConfirmAccount {
                    person: "00000000-0000-4000-8000-000000000002".into(),
                    choice: choice.value()
                }
            )
            .is_err(),
            "same display name grants no identity transfer"
        );
    }
}
