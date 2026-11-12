//! The content column's pages and the dialogs over the window, built from
//! the toolkit's retained compositions. The root builds them again whenever
//! the page they show changes (see [`crate::vault::Core::page_revision`]).

use std::rc::Rc;

use accounts::{SecretItem, SecretItemKind};
use ui::TextRole;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::SpacingStep;
use ui::widget::{
    Actions, Banner, Button, ButtonRole, Checkbox, Column, Dropdown, Element, EmptyState, Field,
    Form, FormSection, LabelPlacement, ListRow, ModalDialog, ObjectHeader, Property, PropertyList,
    Row, Scrollable, Section, SectionHeader, Spacer, Stack, StatGroup, StatTile, Text, TextInput,
    TextInputKind, Tone,
};

use crate::CATALOG;
use crate::actions::PasswordsEffect;
use crate::vault::{
    Core, Dialog, DraftField, Request, Requests, Screen, Shared, identity, identity_label,
    is_favorite, kind_label, origin_label, secret_label,
};
use ui::i18n::ArgValue;

/// Shown in place of a secret that is not revealed.
const MASK: &str = "••••••••••••";

/// The kinds in the editor's switcher, in order.
const KINDS: [(SecretItemKind, &str); 5] = [
    (SecretItemKind::Login, "Login"),
    (SecretItemKind::ApiKey, "API key"),
    (SecretItemKind::SecureNote, "Secure note"),
    (SecretItemKind::Wifi, "Wi-Fi password"),
    (SecretItemKind::Other, "Other secret"),
];

fn ask(requests: &Requests, request: Request) -> impl FnMut() + 'static {
    let requests = Rc::clone(requests);
    move || requests.borrow_mut().push(request.clone())
}

/// `property` with its button running `effect`: the control asks the vault
/// what the palette or an agent asking for the same effect asks. Without a
/// published effect for it the property stays readable, with no button.
fn bound_action(property: Property, icon: Icon, label: &str, effect: &PasswordsEffect) -> Property {
    match crate::bindings::effect(effect) {
        Some(binding) => property.action_binding(icon, label, binding),
        None => property,
    }
}

fn button(
    id: &str,
    label: &str,
    role: ButtonRole,
    requests: &Requests,
    request: Request,
) -> Button {
    Button::new(id, label)
        .role(role)
        .on_effect(ask(requests, request))
}

fn text(value: &str, role: TextRole) -> Element {
    Box::new(Text::new(value).with_role(role))
}

/// A page on the content ground: a vertical scroll of `children`.
fn page(children: Vec<Element>) -> Element {
    let column = children
        .into_iter()
        .fold(Column::new().with_intrinsic_height(), Column::push);
    Box::new(Scrollable::new_vertical().with_child(Box::new(column)))
}

/// A tool page: its title, an optional sentence under it, then `groups`.
/// Groups stand a group gap apart: an empty spacer between two of them
/// takes the column's gap twice.
fn titled_page(title: &str, lead: Option<&str>, groups: Vec<Element>) -> Element {
    let mut children = vec![text(title, TextRole::Title)];
    if let Some(lead) = lead {
        children.push(text(lead, TextRole::Body));
    }
    for group in groups {
        children.push(Box::new(Spacer::token(SpacingStep::S0)));
        children.push(group);
    }
    page(children)
}

/// The content column: the page for the current screen, and the dialog
/// over the window when one is open. While the window bar hosts the
/// window (`actions_in_bar`), what can be done to the item shown is the
/// bar's ([`item_actions`]) and its page draws none of it.
pub(crate) fn content(
    core: &Core,
    shared: &Shared,
    requests: &Requests,
    actions_in_bar: bool,
) -> Element {
    let page = match core.screen {
        Screen::Vault => detail(core, requests, actions_in_bar),
        Screen::Editor => editor(core, shared, requests),
        Screen::Review => review(core),
        Screen::AppAccess => app_access(core, requests),
        Screen::Transfer => transfer(requests),
    };
    match dialog(core, shared, requests) {
        Some(dialog) => Box::new(Stack::new().push(page).push(dialog)),
        None => page,
    }
}

fn detail(core: &Core, requests: &Requests, actions_in_bar: bool) -> Element {
    let Some(item) = core.selected_item() else {
        // The list offers the next step; the wide pane beside it says what
        // the vault keeps instead of standing blank.
        if core.opening {
            return Box::new(
                EmptyState::new("Opening your vault")
                    .with_id("passwords.content.empty")
                    .with_icon(Icon::Shield)
                    .busy(),
            );
        }
        if core.items.is_empty() {
            return Box::new(
                EmptyState::new("Nothing saved yet")
                    .with_id("passwords.content.empty")
                    .with_icon(Icon::Shield)
                    .with_message(CATALOG.t("passwords-empty-content-message")),
            );
        }
        if core.visible().is_empty() {
            return page(Vec::new());
        }
        return Box::new(EmptyState::new("No item selected").with_icon(Icon::Shield));
    };
    let mut children: Vec<Element> = Vec::new();
    if let Some(error) = &core.recovery_error {
        children.push(Box::new(Banner::error(error.clone())));
    }
    children.extend(detail_header(item, requests, actions_in_bar));
    children.push(Box::new(PropertyList::new(properties(core, item))));
    page(children)
}

/// What can be done to the item shown: copy its secret, edit it and mark
/// it a favorite, as the window bar offers them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ItemActions {
    pub(crate) id: accounts::SecretId,
    /// "Copy password", "Copy API key".
    pub(crate) copy: String,
    pub(crate) favorite: bool,
}

/// The actions on the item the vault shows, if it shows one.
pub(crate) fn item_actions(core: &Core) -> Option<ItemActions> {
    if core.screen != Screen::Vault {
        return None;
    }
    let item = core.selected_item()?;
    Some(ItemActions {
        id: item.id.clone(),
        copy: copy_label(item.kind),
        favorite: is_favorite(item),
    })
}

/// The name of copying a secret of `kind`: "Copy password".
fn copy_label(kind: SecretItemKind) -> String {
    format!("Copy {}", secret_noun(kind))
}

/// The name of the favorite toggle, by whether the item is one.
pub(crate) fn favorite_label(favorite: bool) -> String {
    let message = if favorite {
        "passwords-unfavorite"
    } else {
        "passwords-favorite"
    };
    crate::CATALOG.t(message)
}

fn detail_header(item: &SecretItem, requests: &Requests, actions_in_bar: bool) -> Vec<Element> {
    let title = ObjectHeader::new(&item.title)
        .avatar(&item.title, &item.id.0)
        .square()
        .caption(kind_label(item.kind));
    if actions_in_bar {
        return vec![Box::new(title)];
    }
    let favorite = is_favorite(item);
    let copy = copy_label(item.kind);
    let star = {
        let requests = Rc::clone(requests);
        let id = item.id.clone();
        let favorite_label = favorite_label(favorite);
        Button::icon_only("passwords.favorite", Icon::Star, &favorite_label)
            .toggled(favorite)
            .on_own_device_effect(move || {
                requests
                    .borrow_mut()
                    .push(Request::ToggleFavorite(id.clone()));
            })
    };
    let actions = Row::new()
        .push(Box::new(button(
            "passwords.copy",
            &copy,
            ButtonRole::Primary,
            requests,
            Request::CopySecret,
        )))
        .push(Box::new(button(
            "passwords.edit",
            &CATALOG.t("passwords-edit"),
            ButtonRole::Secondary,
            requests,
            Request::Edit,
        )))
        .push(Box::new(star));
    vec![Box::new(title), Box::new(actions)]
}

/// The secret's name inside a sentence: "password", "API key".
fn secret_noun(kind: SecretItemKind) -> String {
    match kind {
        SecretItemKind::ApiKey => "API key".to_owned(),
        _ => secret_label(kind).to_lowercase(),
    }
}

fn properties(core: &Core, item: &SecretItem) -> Vec<Property> {
    let mut rows = Vec::new();
    let who = identity(item);
    if !who.is_empty() && item.kind != SecretItemKind::SecureNote {
        rows.push(bound_action(
            Property::new(identity_label(item.kind), who).with_kind(TextKind::Identifier),
            Icon::Copy,
            "Copy",
            &PasswordsEffect::CopyIdentity {
                item: item.id.0.clone(),
            },
        ));
    }
    let label = secret_label(item.kind);
    let revealed = core.revealed_value(&item.id);
    let shown = revealed.is_some();
    let (value, action, effect) = match revealed {
        Some(value) => (
            value,
            format!("Hide {}", secret_noun(item.kind)),
            PasswordsEffect::Hide {
                item: item.id.0.clone(),
            },
        ),
        None => (
            MASK,
            format!("Show {}", secret_noun(item.kind)),
            PasswordsEffect::Reveal {
                item: item.id.0.clone(),
            },
        ),
    };
    let secret = bound_action(
        Property::new(label, value).with_kind(TextKind::Secret),
        Icon::Eye,
        &action,
        &effect,
    );
    // A masked value reads as hidden, not as a row of dots.
    rows.push(if shown {
        secret
    } else {
        secret.with_accessible_value("Hidden")
    });
    if matches!(item.kind, SecretItemKind::Login | SecretItemKind::ApiKey)
        && !item.origins.is_empty()
    {
        let origins = item.origins.iter().cloned().collect::<Vec<_>>().join("\n");
        rows.push(bound_action(
            Property::new(origin_label(item.kind, item.origins.len()), origins)
                .with_kind(TextKind::Identifier),
            Icon::Copy,
            "Copy",
            &PasswordsEffect::CopyOrigins {
                item: item.id.0.clone(),
            },
        ));
    }
    let tags: Vec<&str> = item
        .tags
        .iter()
        .map(String::as_str)
        .filter(|tag| *tag != "favorite")
        .collect();
    if !tags.is_empty() {
        rows.push(Property::new(CATALOG.t("passwords-tags"), tags.join(", ")));
    }
    if item.kind != SecretItemKind::SecureNote && core.has_private_note(&item.id) {
        rows.push(Property::new(
            CATALOG.t("passwords-notes"),
            "Hidden until you edit the item",
        ));
    }
    let ago = crate::vault::now_unix().saturating_sub(item.updated_unix);
    rows.push(Property::new(
        CATALOG.t("passwords-last-changed"),
        ui::format::relative(i64::try_from(ago).unwrap_or(i64::MAX)),
    ));
    rows
}

fn input(shared: &Shared, field: DraftField, value: &str) -> TextInput {
    let core = Rc::clone(shared);
    let id = match field {
        DraftField::Name => "passwords.field.name",
        DraftField::Username => "passwords.field.username",
        DraftField::Origin => "passwords.field.origin",
        DraftField::Password => "passwords.field.password",
        DraftField::Notes => "passwords.field.notes",
        DraftField::Tags => "passwords.field.tags",
        DraftField::CustomFields => "passwords.field.custom-fields",
    };
    let input = TextInput::local().with_id(id);
    let input = match field {
        DraftField::Name => input.with_kind(TextInputKind::Name),
        DraftField::Username => input.with_kind(TextInputKind::Username),
        DraftField::Origin => input
            .with_kind(TextInputKind::Url)
            .with_placeholder("https://example.com"),
        DraftField::Password => input.with_secret(true),
        DraftField::Notes => input
            .with_min_lines(4)
            .with_placeholder(crate::CATALOG.t("passwords-add-note")),
        DraftField::Tags => input.with_placeholder(crate::CATALOG.t("passwords-tags-hint")),
        DraftField::CustomFields => input
            .with_min_lines(3)
            .with_placeholder(crate::CATALOG.t("passwords-custom-fields-hint")),
    };
    input
        .with_initial_value(value)
        .on_change_view(move |value| core.borrow_mut().set_draft(field, value))
}

fn field_label(kind: SecretItemKind, field: DraftField) -> String {
    match field {
        DraftField::Name => crate::CATALOG.t("passwords-field-name"),
        DraftField::Username => identity_label(kind),
        DraftField::Origin => origin_label(kind, 2),
        DraftField::Password => secret_label(kind).to_owned(),
        DraftField::Notes if kind == SecretItemKind::SecureNote => {
            crate::CATALOG.t("passwords-field-note")
        }
        DraftField::Notes => crate::CATALOG.t("passwords-field-notes"),
        DraftField::Tags => crate::CATALOG.t("passwords-field-tags"),
        DraftField::CustomFields => crate::CATALOG.t("passwords-field-custom"),
    }
}

fn draft_field(core: &Core, shared: &Shared, field: DraftField) -> Field {
    let kind = core.draft_kind;
    let field_widget = Field::new(
        field_label(kind, field),
        input(shared, field, core.draft(field)),
    );
    match &core.draft_error {
        Some((failed, message)) if *failed == field => field_widget.with_error(message.clone()),
        _ if field == DraftField::Password && core.editing.is_some() => {
            field_widget.with_help("Leave it empty to keep the current one")
        }
        _ => field_widget,
    }
}

fn editor(core: &Core, shared: &Shared, requests: &Requests) -> Element {
    let kind = core.draft_kind;
    // A list, not a segmented strip: five kinds never fit an editor pane
    // beside the item list without cutting their names short.
    let selected = KINDS
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map_or("Login", |(_, label)| *label);
    let kinds = {
        let requests = Rc::clone(requests);
        Dropdown::from_state(
            KINDS.iter().map(|(_, label)| (*label).to_owned()).collect(),
            &ui::LocalState::new(selected.to_owned()),
        )
        .with_id("passwords.kind")
        .on_change_view(move |label| {
            if let Some((kind, _)) = KINDS.iter().find(|(_, candidate)| *candidate == label) {
                requests.borrow_mut().push(Request::SetKind(*kind));
            }
        })
    };
    let fields = core.draft_fields();
    let mut details = FormSection::new("Details").field(Field::new("Kind", kinds));
    for field in fields.iter().copied().filter(|field| {
        matches!(
            field,
            DraftField::Name | DraftField::Username | DraftField::Origin | DraftField::Password
        )
    }) {
        details = details.field(draft_field(core, shared, field));
        if field == DraftField::Password && kind != SecretItemKind::ApiKey {
            details = details.content(Box::new(button(
                "passwords.generate",
                &CATALOG.t("passwords-generate"),
                ButtonRole::Secondary,
                requests,
                Request::Generate,
            )));
        }
    }
    let mut more = FormSection::new(CATALOG.t(if kind == SecretItemKind::SecureNote {
        "passwords-note"
    } else {
        "passwords-notes-and-tags"
    }));
    for field in [
        DraftField::Notes,
        DraftField::Tags,
        DraftField::CustomFields,
    ] {
        more = more.field(draft_field(core, shared, field));
    }
    let mut actions = Actions::new(button(
        "passwords.save",
        &CATALOG.t(if core.editing.is_some() {
            "passwords-save-changes"
        } else {
            "passwords-save-item"
        }),
        ButtonRole::Primary,
        requests,
        Request::Save,
    ));
    if core.editing.is_some() {
        actions = actions.secondary(button(
            "passwords.delete",
            &CATALOG.t("passwords-delete"),
            ButtonRole::Destructive,
            requests,
            Request::Delete,
        ));
    }
    actions = actions.secondary(button(
        "passwords.cancel",
        &CATALOG.t("passwords-cancel"),
        ButtonRole::Secondary,
        requests,
        Request::CancelEdit,
    ));
    let form = Form::new()
        .with_label_placement(Some(LabelPlacement::Above))
        .section(details)
        .section(more)
        .with_actions(actions);
    let title = CATALOG.t(if core.editing.is_some() {
        "passwords-edit-item"
    } else {
        "passwords-new-item-title"
    });
    titled_page(&title, None, vec![Box::new(form)])
}

fn count_tone(count: usize) -> Tone {
    if count == 0 {
        Tone::Success
    } else {
        Tone::Warning
    }
}

fn review(core: &Core) -> Element {
    let health = core.health;
    // Every tile is a finding with its verdict; how many were checked is a
    // fact about the review below.
    let tiles: Vec<Element> = vec![
        Box::new(
            StatTile::new("Reused", health.reused.to_string()).tone(count_tone(health.reused)),
        ),
        Box::new(StatTile::new("Weak", health.weak.to_string()).tone(count_tone(health.weak))),
        Box::new(
            StatTile::new("Not changed in a year", health.older_than_year.to_string())
                .tone(count_tone(health.older_than_year)),
        ),
    ];
    // With nothing saved there is nothing to grade: no green tiles.
    let mut groups: Vec<Element> = if health.total == 0 {
        Vec::new()
    } else {
        vec![Box::new(StatGroup::new(tiles))]
    };
    let rows: Vec<Element> = core
        .findings
        .iter()
        .filter_map(|finding| {
            let item = core.item(&finding.id)?;
            let row = ListRow::new(format!("review:{}", item.id.0), item.title.clone())
                .icon(crate::search::icon(item.kind))
                .secondary(finding.reasons())
                .disclosure();
            // The item's published `open`, by its opaque id alone.
            let published = crate::actions::item_row(item).into_resource();
            Some(
                Box::new(match crate::actions::item_binding(&published, "open") {
                    Some(open) => row.on_action(open),
                    None => row,
                }) as Element,
            )
        })
        .collect();
    if health.total == 0 {
        groups.push(Box::new(
            Section::new(vec![text(
                &CATALOG.t("passwords-review-nothing-to-check"),
                TextRole::Body,
            )])
            .with_header(SectionHeader::group(CATALOG.t("passwords-review-empty"))),
        ));
    } else if rows.is_empty() {
        groups.push(Box::new(
            Section::new(vec![text(
                &CATALOG.t("passwords-review-all-well"),
                TextRole::Body,
            )])
            .with_header(SectionHeader::group(CATALOG.t("passwords-review-done"))),
        ));
    } else {
        let count = rows.len();
        groups.push(Box::new(Section::new(rows).with_header(
            SectionHeader::group(CATALOG.t("passwords-review-attention")).count(count),
        )));
    }
    let mut facts = vec![
        Property::new(CATALOG.t("passwords-checked"), health.total.to_string())
            .with_kind(TextKind::Numeric),
        Property::new(CATALOG.t("passwords-checked-against"), CHECKED_AGAINST),
    ];
    if let Some(custody) = core.custody {
        facts.push(Property::new(
            CATALOG.t("passwords-encryption-key"),
            custody,
        ));
    }
    groups.push(Box::new(
        Section::new(vec![Box::new(PropertyList::new(facts))])
            .with_header(SectionHeader::group(CATALOG.t("passwords-review-about"))),
    ));
    titled_page(&CATALOG.t("passwords-review"), None, groups)
}

/// What the review compares against: only this vault, wherever it is kept (a
/// browser's storage or the device), so the words name the vault, not a place.
const CHECKED_AGAINST: &str = "Only the passwords in this vault";

fn app_access(core: &Core, requests: &Requests) -> Element {
    if core.app_grants.is_empty() && core.app_proposals.is_empty() {
        return Box::new(
            EmptyState::new("No app can use your vault without asking").with_icon(Icon::Shield),
        );
    }
    let grants: Vec<Property> = core
        .app_grants
        .iter()
        .map(|grant| grant_property(core, grant))
        .collect();
    let count = grants.len();
    let mut sections: Vec<Element> = Vec::new();
    if !core.app_proposals.is_empty() {
        let pending = core
            .app_proposals
            .iter()
            .map(|proposal| {
                let review = |lifetime: &str, label: &str| {
                    button(
                        &format!("passwords.review-request:{}:{lifetime}", proposal.id),
                        &crate::CATALOG.t(label),
                        ButtonRole::Secondary,
                        requests,
                        Request::ReviewAccess {
                            proposal: proposal.id.clone(),
                            lifetime: lifetime.into(),
                        },
                    )
                };
                let buttons = Actions::new(review("session", "passwords-allow-session"))
                    .secondary(review("persistent", "passwords-allow-persistent"))
                    .secondary(button(
                        &format!("passwords.deny-request:{}", proposal.id),
                        &crate::CATALOG.t("passwords-deny-access"),
                        ButtonRole::Secondary,
                        requests,
                        Request::DenyAccess(proposal.id.clone()),
                    ));
                Box::new(
                    Column::new()
                        .with_intrinsic_height()
                        .push(text(&proposal_details(core, proposal), TextRole::Body))
                        .push(Box::new(buttons)),
                ) as Element
            })
            .collect();
        sections.push(Box::new(
            Section::new(pending).with_header(
                SectionHeader::group(crate::CATALOG.t("passwords-pending-access"))
                    .count(core.app_proposals.len()),
            ),
        ));
    }
    if !grants.is_empty() {
        sections.push(Box::new(
            Section::new(vec![Box::new(PropertyList::new(grants))]).with_header(
                SectionHeader::group(CATALOG.t("passwords-allowed-apps")).count(count),
            ),
        ));
    }
    titled_page(
        &CATALOG.t("passwords-app-access"),
        Some(&CATALOG.t("passwords-app-access-lead")),
        sections,
    )
}

fn grant_property(core: &Core, grant: &accounts::CredentialGrant) -> Property {
    let item = core
        .item(&grant.item_id)
        .map_or_else(|| "A saved item".to_owned(), |item| item.title.clone());
    let relationship = match &grant.origin {
        Some(origin) => format!("{item}\n{}", crate::vault::host_of(origin)),
        None => item,
    };
    let lifetime = crate::CATALOG.t(match grant.lifetime {
        accounts::GrantLifetime::Session => "passwords-session-access",
        accounts::GrantLifetime::Persistent => "passwords-persistent-access",
    });
    let scope = grant.scope.as_ref().map(|scope| {
        crate::CATALOG.t_args(
            "passwords-account-scope",
            &[
                (
                    "service",
                    ui::i18n::ArgValue::Text(
                        crate::CATALOG.t(&format!("passwords-service-{}", scope.service)),
                    ),
                ),
                ("account", ui::i18n::ArgValue::Text(scope.account.clone())),
                (
                    "generation",
                    ui::i18n::ArgValue::Text(scope.generation.to_string()),
                ),
            ],
        )
    });
    let value = scope.map_or_else(
        || format!("{relationship}\n{lifetime}"),
        |scope| format!("{relationship}\n{scope}\n{lifetime}"),
    );
    bound_action(
        Property::new(crate::vault::app_name(&grant.app_id), value),
        Icon::Ban,
        &crate::CATALOG.t("passwords-revoke-access"),
        &PasswordsEffect::RevokeAccess {
            grant: grant.id().into(),
        },
    )
}

fn proposal_details(core: &Core, proposal: &accounts::CredentialProposal) -> String {
    let item = core.item(&proposal.request.item_id).map_or_else(
        || crate::CATALOG.t("passwords-request-item"),
        |item| item.title.clone(),
    );
    let fields = [
        ("app", crate::vault::app_name(&proposal.request.app_id)),
        ("item", item),
        ("purpose", proposal.request.purpose.clone()),
        (
            "origin",
            proposal.request.origin.clone().unwrap_or_default(),
        ),
        (
            "service",
            crate::CATALOG.t(&format!("passwords-service-{}", proposal.scope.service)),
        ),
        ("account", proposal.scope.account.clone()),
        ("generation", proposal.scope.generation.to_string()),
        ("principal", proposal.scope.principal.clone()),
        ("device", proposal.scope.device.clone()),
    ];
    crate::CATALOG.t_args(
        "passwords-request-details",
        &fields
            .into_iter()
            .map(|(key, value)| (key, ui::i18n::ArgValue::Text(value)))
            .collect::<Vec<_>>(),
    )
}

/// A titled group inside a page.
fn group(title: &str, items: Vec<Element>) -> Section {
    Section::new(items).with_header(SectionHeader::group(title))
}

fn transfer(requests: &Requests) -> Element {
    let pair = |first: Button, second: Button| -> Element {
        Box::new(Row::new().push(Box::new(first)).push(Box::new(second)))
    };
    titled_page(
        &CATALOG.t("passwords-import-export"),
        None,
        vec![
            Box::new(group(
                &CATALOG.t("passwords-encrypted-backup"),
                vec![
                    text(&CATALOG.t("passwords-backup-lead"), TextRole::Body),
                    pair(
                        button(
                            "passwords.backup",
                            &CATALOG.t("passwords-back-up"),
                            ButtonRole::Secondary,
                            requests,
                            Request::Backup,
                        ),
                        button(
                            "passwords.restore",
                            &CATALOG.t("passwords-restore"),
                            ButtonRole::Secondary,
                            requests,
                            Request::Restore,
                        ),
                    ),
                ],
            )),
            Box::new(group(
                &CATALOG.t("passwords-browser-passwords"),
                vec![
                    text(&CATALOG.t("passwords-csv-lead"), TextRole::Body),
                    pair(
                        button(
                            "passwords.import",
                            &CATALOG.t("passwords-import-csv"),
                            ButtonRole::Secondary,
                            requests,
                            Request::Import,
                        ),
                        button(
                            "passwords.export",
                            &CATALOG.t("passwords-export-csv"),
                            ButtonRole::Secondary,
                            requests,
                            Request::Export,
                        ),
                    ),
                ],
            )),
        ],
    )
}

fn body(children: Vec<Element>) -> Column {
    children.into_iter().fold(
        Column::new().with_no_padding().with_intrinsic_height(),
        Column::push,
    )
}

fn secret_input(shared: &Shared, confirmation: bool, label: &str, error: Option<&str>) -> Field {
    let core = Rc::clone(shared);
    let value = {
        let core = shared.borrow();
        if confirmation {
            core.backup_confirmation.to_string()
        } else {
            core.backup_password.to_string()
        }
    };
    let input = TextInput::local()
        .with_id(if confirmation {
            "passwords.backup.confirmation"
        } else {
            "passwords.backup.password"
        })
        .with_secret(true)
        .with_initial_value(&value)
        .on_change_view(move |value| {
            let mut core = core.borrow_mut();
            let before = core.backup_confirmation_problem();
            let slot = if confirmation {
                &mut core.backup_confirmation
            } else {
                &mut core.backup_password
            };
            *slot = zeroize::Zeroizing::new(value.to_owned());
            // The form is built again only when what it says changes; the
            // fields keep their caret by id.
            if core.backup_confirmation_problem() != before {
                core.touch_page();
            }
        });
    let field = Field::new(label, input).with_label_placement(Some(LabelPlacement::Above));
    let live = if confirmation {
        shared.borrow().backup_confirmation_problem()
    } else {
        None
    };
    match error.or(live) {
        Some(error) => field.with_error(error),
        None if !confirmation => field.with_help("Use at least 12 characters"),
        None => field,
    }
}

fn import_dialog(
    rows: &[crate::transfer::ImportLogin],
    skipped: &[bool],
    requests: &Requests,
) -> ModalDialog<Column> {
    let (new, known): (Vec<_>, Vec<_>) = rows
        .iter()
        .zip(skipped.iter().copied().chain(std::iter::repeat(false)))
        .partition(|(_, skip)| !*skip);
    let mut children: Vec<Element> = Vec::new();
    match known.as_slice() {
        [] => {}
        [(row, _)] => children.push(text(
            &CATALOG.t_args(
                "passwords-import-skipped-one",
                &[("login", ui::i18n::ArgValue::Text((*row.title).clone()))],
            ),
            TextRole::Body,
        )),
        more => children.push(text(
            &CATALOG.t_args(
                "passwords-import-skipped-count",
                &[(
                    "count",
                    ui::i18n::ArgValue::from(u32::try_from(more.len()).unwrap_or(u32::MAX)),
                )],
            ),
            TextRole::Body,
        )),
    }
    let listed: Vec<Element> = new
        .iter()
        .take(5)
        .enumerate()
        .map(|(index, (row, _))| {
            Box::new(
                ListRow::new(format!("import:{index}"), row.title.to_string())
                    .icon(Icon::Globe)
                    .secondary(row.username.to_string()),
            ) as Element
        })
        .collect();
    if !listed.is_empty() {
        children.push(Box::new(Section::new(listed)));
    }
    if new.len() > 5 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a count only selects a plural form; exactness past 2^52 is irrelevant"
        )]
        let number = new.len().saturating_sub(5) as f64;
        children.push(Box::new(
            Text::new(
                &CATALOG.t_args("passwords-and-more", &[("count", ArgValue::Number(number))]),
            )
            .with_role(TextRole::Caption)
            .dimmed(),
        ));
    }
    let title = match new.len() {
        0 => CATALOG.t("passwords-import-nothing"),
        1 => CATALOG.t("passwords-import-one"),
        n => CATALOG.t_args(
            "passwords-import-count",
            &[("count", ui::i18n::ArgValue::Text(n.to_string()))],
        ),
    };
    ModalDialog::new(title, body(children))
        .primary_action("Import", ask(requests, Request::ConfirmImport))
        .secondary_action("Cancel", ask(requests, Request::CloseDialog))
}

fn export_dialog(acknowledged: bool, reminded: bool, requests: &Requests) -> ModalDialog<Column> {
    let ack = {
        let requests = Rc::clone(requests);
        Checkbox::from_state(&ui::LocalState::new(acknowledged))
            .with_label(crate::CATALOG.t("passwords-export-acknowledge"))
            .on_view("passwords-export-acknowledge", move |on| {
                requests.borrow_mut().push(Request::Acknowledge(on));
            })
    };
    let mut children: Vec<Element> = vec![
        text(&CATALOG.t("passwords-export-warning"), TextRole::Body),
        Box::new(ack),
    ];
    if reminded {
        children.push(Box::new(
            Text::new(&crate::CATALOG.t("passwords-confirm-export")).with_role(TextRole::Caption),
        ));
    }
    ModalDialog::new("Export passwords to a CSV file?", body(children))
        .primary_action("Choose file…", ask(requests, Request::ConfirmExport))
        .with_primary_enabled(acknowledged)
        .secondary_action("Cancel", ask(requests, Request::CloseDialog))
}

/// The backup form. Its values live in the shared core, and "Choose file…"
/// waits there until the password is long enough and confirmed.
fn backup_dialog(
    shared: &Shared,
    error: Option<&'static str>,
    requests: &Requests,
) -> ModalDialog<Column> {
    let fields: Vec<Element> = vec![
        Box::new(secret_input(shared, false, "Backup password", None)),
        Box::new(secret_input(shared, true, "Confirm backup password", error)),
    ];
    let core = Rc::clone(shared);
    ModalDialog::new("Back up vault", body(fields))
        .primary_action("Choose file…", ask(requests, Request::ConfirmBackup))
        .primary_enabled_if(move |_| core.borrow().backup_form_error(true).is_none())
}

fn dialog(core: &Core, shared: &Shared, requests: &Requests) -> Option<Element> {
    let dialog = core.dialog.as_ref()?;
    let close = ask(requests, Request::CloseDialog);
    let built = match dialog {
        Dialog::ImportPreview { rows, skipped } => import_dialog(rows, skipped, requests),
        Dialog::ExportPlaintext {
            acknowledged,
            reminded,
        } => export_dialog(*acknowledged, *reminded, requests),
        Dialog::Backup { error } => {
            backup_dialog(shared, *error, requests).secondary_action("Cancel", close)
        }
        Dialog::Restore { error, .. } => {
            let field: Element = Box::new(secret_input(
                shared,
                false,
                "Backup password",
                error.as_deref(),
            ));
            ModalDialog::new("Restore from backup", body(vec![field]))
                .primary_action("Restore", ask(requests, Request::ConfirmRestore))
                .secondary_action("Cancel", close)
        }
        Dialog::DeleteConfirm { title, .. } => ModalDialog::new(
            format!("Delete “{title}”?"),
            body(vec![text(
                &CATALOG.t("passwords-delete-lead"),
                TextRole::Body,
            )]),
        )
        .with_destructive(true)
        .primary_action("Delete item", ask(requests, Request::ConfirmDelete))
        .secondary_action("Cancel", close),
        Dialog::AccessConfirm { proposal, lifetime } => {
            let pending = core
                .app_proposals
                .iter()
                .find(|pending| pending.id == *proposal);
            let description = pending.map_or_else(
                || crate::CATALOG.t("passwords-request-expired"),
                |pending| proposal_details(core, pending),
            );
            let policy = crate::CATALOG.t(match lifetime.as_str() {
                "persistent" => "passwords-persistent-access",
                _ => "passwords-session-access",
            });
            ModalDialog::new(
                crate::CATALOG.t("passwords-review-access"),
                body(vec![
                    text(&description, TextRole::Body),
                    text(&policy, TextRole::Body),
                ]),
            )
            .with_primary_enabled(pending.is_some())
            .primary_action(
                crate::CATALOG.t("passwords-approve-access"),
                ask(requests, Request::ConfirmAccess),
            )
            .secondary_action(crate::CATALOG.t("passwords-access-cancel"), close)
        }
    };
    Some(Box::new(
        built
            .with_test_id(format!("passwords.dialog.{}", dialog.kind()))
            .on_dismiss(ask(requests, Request::CloseDialog))
            .open(true),
    ))
}

#[cfg(test)]
mod backup_form_tests {
    use super::*;
    use std::cell::RefCell;
    use zeroize::Zeroizing;

    /// "Choose file…" waits for a long, confirmed password in the shared
    /// core, and the confirmation field says so while it is typed.
    #[test]
    fn the_backup_form_gates_its_file_choice_and_checks_inline() {
        let shared: Shared = Rc::new(RefCell::new(Core::demo()));
        let requests: Requests = Rc::default();
        let dialog = backup_dialog(&shared, None, &requests);
        assert!(!dialog.primary_enabled(), "an empty form cannot go on");

        shared.borrow_mut().backup_password = Zeroizing::new("short".into());
        assert!(!dialog.primary_enabled(), "too short");

        shared.borrow_mut().backup_password = Zeroizing::new("a memorable backup password".into());
        shared.borrow_mut().backup_confirmation = Zeroizing::new("a memorable".into());
        assert_eq!(
            shared.borrow().backup_confirmation_problem(),
            None,
            "a confirmation on its way to the password is not an error yet"
        );
        assert!(!dialog.primary_enabled(), "not confirmed yet");

        shared.borrow_mut().backup_confirmation = Zeroizing::new("a mistake".into());
        assert_eq!(
            shared.borrow().backup_confirmation_problem(),
            Some("The passwords do not match")
        );

        shared.borrow_mut().backup_confirmation =
            Zeroizing::new("a memorable backup password".into());
        assert!(dialog.primary_enabled(), "a confirmed password can go on");
    }
}
