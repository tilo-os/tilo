//! The content column's pages and the dialogs over the window, built from
//! the toolkit's retained compositions. The root builds them again whenever
//! the page they show changes (see [`crate::vault::Core::page_revision`]).

use std::rc::Rc;

use accounts::{SecretItem, SecretItemKind};
use ui::TextRole;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Actions, Avatar, AvatarSize, Banner, Button, ButtonRole, Checkbox, Column, Element, EmptyState,
    Field, Form, FormPage, FormSection, LabelPlacement, ListRow, ModalDialog, Property,
    PropertyList, Row, Scrollable, Section, SectionHeader, Stack, StatGroup, StatTile, Text,
    TextInput, TextInputKind, ToggleGroup, Tone,
};

use crate::vault::{
    Core, Dialog, DraftField, Request, Requests, Screen, Shared, identity, identity_label,
    is_favorite, origin_label, secret_label,
};

/// Shown in place of a secret that is not revealed.
const MASK: &str = "••••••••••••";

/// The kinds in the editor's switcher, in order.
const KINDS: [(SecretItemKind, &str); 5] = [
    (SecretItemKind::Login, "Login"),
    (SecretItemKind::ApiKey, "API key"),
    (SecretItemKind::SecureNote, "Note"),
    (SecretItemKind::Wifi, "Wi-Fi"),
    (SecretItemKind::Other, "Other"),
];

fn ask(requests: &Requests, request: Request) -> impl FnMut() + 'static {
    let requests = Rc::clone(requests);
    move || requests.borrow_mut().push(request.clone())
}

fn button(
    id: &str,
    label: &str,
    role: ButtonRole,
    requests: &Requests,
    request: Request,
) -> Button {
    Button::new(id, label, None)
        .role(role)
        .on_click(ask(requests, request))
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

/// The content column: the page for the current screen, and the dialog
/// over the window when one is open.
pub(crate) fn content(core: &Core, shared: &Shared, requests: &Requests) -> Element {
    let page = match core.screen {
        Screen::Vault => detail(core, requests),
        Screen::Editor => editor(core, shared, requests),
        Screen::Review => review(core, requests),
        Screen::AppAccess => app_access(core, requests),
        Screen::Transfer => transfer(requests),
    };
    match dialog(core, shared, requests) {
        Some(dialog) => Box::new(Stack::new().push(page).push(dialog)),
        None => page,
    }
}

// --- the open item ------------------------------------------------------

fn detail(core: &Core, requests: &Requests) -> Element {
    let Some(item) = core.selected_item() else {
        let title = if core.items.is_empty() {
            "Items you save appear here"
        } else {
            "No item selected"
        };
        return Box::new(EmptyState::new(title).with_icon(Icon::Shield));
    };
    let mut children: Vec<Element> = Vec::new();
    if let Some(error) = &core.recovery_error {
        children.push(Box::new(Banner::error(error.clone())));
    }
    children.extend(detail_header(item, requests));
    children.push(Box::new(PropertyList::new(properties(
        core, item, requests,
    ))));
    page(children)
}

fn detail_header(item: &SecretItem, requests: &Requests) -> Vec<Element> {
    let favorite = is_favorite(item);
    let copy = format!("Copy {}", secret_noun(item.kind));
    // An icon button has no "on" face, so the label says which way it goes.
    let star = button(
        "passwords.favorite",
        if favorite {
            "Remove from favorites"
        } else {
            "Add to favorites"
        },
        ButtonRole::Plain,
        requests,
        Request::ToggleFavorite(item.id.clone()),
    )
    .with_icon(Icon::Star);
    let title = Row::new()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(
            Avatar::person(&item.title, &item.id.0)
                .square()
                .size(AvatarSize::Large),
        ))
        .push(text(&item.title, TextRole::Title));
    let actions = Row::new()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(button(
            "passwords.copy",
            &copy,
            ButtonRole::Primary,
            requests,
            Request::CopySecret,
        )))
        .push(Box::new(button(
            "passwords.edit",
            "Edit",
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

fn properties(core: &Core, item: &SecretItem, requests: &Requests) -> Vec<Property> {
    let mut rows = Vec::new();
    let who = identity(item);
    if !who.is_empty() && item.kind != SecretItemKind::SecureNote {
        rows.push(
            Property::new(identity_label(item.kind), who)
                .with_kind(TextKind::Identifier)
                .action(Icon::Copy, "Copy", ask(requests, Request::CopyIdentity)),
        );
    }
    let label = secret_label(item.kind);
    let revealed = core.revealed_value(&item.id);
    let (value, action) = match revealed {
        Some(value) => (value, format!("Hide {}", secret_noun(item.kind))),
        None => (MASK, format!("Show {}", secret_noun(item.kind))),
    };
    rows.push(
        Property::new(label, value)
            .with_kind(TextKind::Secret)
            .action(Icon::Eye, action, ask(requests, Request::RevealSecret)),
    );
    if matches!(item.kind, SecretItemKind::Login | SecretItemKind::ApiKey)
        && !item.origins.is_empty()
    {
        let origins = item.origins.iter().cloned().collect::<Vec<_>>().join("\n");
        rows.push(
            Property::new(origin_label(item.kind, item.origins.len()), origins)
                .with_kind(TextKind::Identifier)
                .action(Icon::Copy, "Copy", ask(requests, Request::CopyOrigins)),
        );
    }
    let tags: Vec<&str> = item
        .tags
        .iter()
        .map(String::as_str)
        .filter(|tag| *tag != "favorite")
        .collect();
    if !tags.is_empty() {
        rows.push(Property::new("Tags", tags.join(", ")));
    }
    if item.kind != SecretItemKind::SecureNote && core.has_private_note(&item.id) {
        rows.push(Property::new("Notes", "Hidden until you edit the item"));
    }
    let ago = crate::vault::now_unix().saturating_sub(item.updated_unix);
    rows.push(Property::new(
        "Last changed",
        ui::format::relative(i64::try_from(ago).unwrap_or(i64::MAX)),
    ));
    rows
}

// --- the editor ---------------------------------------------------------

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
        DraftField::Notes => input.multiline(true),
        DraftField::Tags => input.with_placeholder("Separate tags with commas"),
        DraftField::CustomFields => input
            .multiline(true)
            .with_placeholder("One per line, such as PIN = 1234"),
    };
    input
        .with_initial_value(value)
        .on_change(move |value| core.borrow_mut().set_draft(field, value))
}

fn field_label(kind: SecretItemKind, field: DraftField) -> &'static str {
    match field {
        DraftField::Name => "Name",
        DraftField::Username => identity_label(kind),
        DraftField::Origin => origin_label(kind, 2),
        DraftField::Password => secret_label(kind),
        DraftField::Notes if kind == SecretItemKind::SecureNote => "Note",
        DraftField::Notes => "Notes",
        DraftField::Tags => "Tags",
        DraftField::CustomFields => "Custom fields",
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
    let selected = KINDS
        .iter()
        .position(|(candidate, _)| *candidate == kind)
        .unwrap_or(0);
    let kinds = {
        let requests = Rc::clone(requests);
        ToggleGroup::new(
            KINDS
                .iter()
                .map(|(_, label)| Segment::new(*label))
                .collect(),
            selected,
        )
        .with_id("passwords.kind")
        .on_change(move |index| {
            if let Some((kind, _)) = KINDS.get(index) {
                requests.borrow_mut().push(Request::SetKind(*kind));
            }
        })
    };
    let fields = core.draft_fields();
    let mut details = FormSection::new("Details").content(Box::new(kinds));
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
                "Generate strong password",
                ButtonRole::Secondary,
                requests,
                Request::Generate,
            )));
        }
    }
    let mut more = FormSection::new(if kind == SecretItemKind::SecureNote {
        "Note"
    } else {
        "Notes and tags"
    });
    for field in [
        DraftField::Notes,
        DraftField::Tags,
        DraftField::CustomFields,
    ] {
        more = more.field(draft_field(core, shared, field));
    }
    let mut actions = Actions::new(button(
        "passwords.save",
        if core.editing.is_some() {
            "Save changes"
        } else {
            "Save item"
        },
        ButtonRole::Primary,
        requests,
        Request::Save,
    ));
    if core.editing.is_some() {
        actions = actions.secondary(button(
            "passwords.delete",
            "Delete item",
            ButtonRole::Destructive,
            requests,
            Request::Delete,
        ));
    }
    actions = actions.secondary(button(
        "passwords.cancel",
        "Cancel",
        ButtonRole::Secondary,
        requests,
        Request::CancelEdit,
    ));
    let form = Form::new()
        .with_label_placement(Some(LabelPlacement::Above))
        .section(details)
        .section(more)
        .with_actions(actions);
    let title = if core.editing.is_some() {
        "Edit item"
    } else {
        "New item"
    };
    Box::new(FormPage::new(title, form))
}

// --- password review ----------------------------------------------------

fn kind_icon(kind: SecretItemKind) -> Icon {
    match kind {
        SecretItemKind::Login => Icon::Globe,
        SecretItemKind::ApiKey => Icon::Code,
        SecretItemKind::SecureNote => Icon::FileText,
        SecretItemKind::Wifi => Icon::Wifi,
        SecretItemKind::Other => Icon::Shield,
    }
}

fn count_tone(count: usize) -> Tone {
    if count == 0 {
        Tone::Success
    } else {
        Tone::Warning
    }
}

fn review(core: &Core, requests: &Requests) -> Element {
    let health = core.health;
    let tiles: Vec<Element> = vec![
        Box::new(StatTile::new("Passwords checked", health.total.to_string())),
        Box::new(
            StatTile::new("Reused", health.reused.to_string()).tone(count_tone(health.reused)),
        ),
        Box::new(StatTile::new("Weak", health.weak.to_string()).tone(count_tone(health.weak))),
        Box::new(
            StatTile::new("Not changed in a year", health.older_than_year.to_string())
                .tone(count_tone(health.older_than_year)),
        ),
    ];
    let mut children: Vec<Element> = vec![
        text("Password review", TextRole::Title),
        Box::new(StatGroup::new(tiles)),
    ];
    let rows: Vec<Element> = core
        .findings
        .iter()
        .filter_map(|finding| {
            let item = core.item(&finding.id)?;
            Some(Box::new(
                ListRow::new(format!("review:{}", item.id.0), item.title.clone())
                    .icon(kind_icon(item.kind))
                    .secondary(finding.reasons())
                    .disclosure()
                    .on_activate(ask(requests, Request::Open(item.id.clone()))),
            ) as Element)
        })
        .collect();
    if rows.is_empty() {
        children.push(Box::new(
            Section::new(vec![text(
                "Every saved password is unique, strong and recent.",
                TextRole::Body,
            )])
            .with_header(SectionHeader::group("Nothing to fix")),
        ));
    } else {
        let count = rows.len();
        children.push(Box::new(
            Section::new(rows).with_header(SectionHeader::group("Needs attention").count(count)),
        ));
    }
    let mut facts = vec![Property::new(
        "Checked against",
        "Only the passwords saved here, on this device",
    )];
    if let Some(custody) = core.custody {
        facts.push(Property::new("Encryption key", custody));
    }
    children.push(Box::new(
        Section::new(vec![Box::new(PropertyList::new(facts))])
            .with_header(SectionHeader::group("About this review")),
    ));
    page(children)
}

// --- app access ---------------------------------------------------------

fn app_access(core: &Core, requests: &Requests) -> Element {
    if core.app_grants.is_empty() {
        return Box::new(
            EmptyState::new("No app can use your vault without asking").with_icon(Icon::Shield),
        );
    }
    let rows: Vec<Element> = core
        .app_grants
        .iter()
        .enumerate()
        .map(|(index, grant)| {
            let item = core
                .item(&grant.item_id)
                .map_or_else(|| "A saved item".to_owned(), |item| item.title.clone());
            let mut row = ListRow::new(
                format!("grant:{index}"),
                crate::vault::app_name(&grant.app_id),
            )
            .icon(Icon::LayoutGrid)
            .secondary(item)
            .action(
                Icon::Ban,
                "Revoke access",
                ask(requests, Request::Revoke(index)),
            );
            if let Some(origin) = &grant.origin {
                row = row.meta(crate::vault::host_of(origin));
            }
            Box::new(row) as Element
        })
        .collect();
    let count = rows.len();
    page(vec![
        text("App access", TextRole::Title),
        text(
            "These apps can fill the item shown without asking you first.",
            TextRole::Body,
        ),
        Box::new(Section::new(rows).with_header(SectionHeader::group("Allowed apps").count(count))),
    ])
}

// --- import and export ---------------------------------------------------

/// A titled group inside a page.
fn group(title: &str, items: Vec<Element>) -> Section {
    Section::new(items).with_header(SectionHeader::group(title))
}

fn transfer(requests: &Requests) -> Element {
    let pair = |first: Button, second: Button| -> Element {
        Box::new(Row::new().push(Box::new(first)).push(Box::new(second)))
    };
    page(vec![
        text("Import and export", TextRole::Title),
        Box::new(group(
            "Encrypted backup",
            vec![
                text(
                    "A backup keeps every item, note and custom field, encrypted with a password you choose.",
                    TextRole::Body,
                ),
                pair(
                    button(
                        "passwords.backup",
                        "Back up vault…",
                        ButtonRole::Secondary,
                        requests,
                        Request::Backup,
                    ),
                    button(
                        "passwords.restore",
                        "Restore from backup…",
                        ButtonRole::Secondary,
                        requests,
                        Request::Restore,
                    ),
                ),
            ],
        )),
        Box::new(group(
            "Browser passwords",
            vec![
                text(
                    "Move logins between Passwords and a web browser with a CSV file.",
                    TextRole::Body,
                ),
                pair(
                    button(
                        "passwords.import",
                        "Import from CSV…",
                        ButtonRole::Secondary,
                        requests,
                        Request::Import,
                    ),
                    button(
                        "passwords.export",
                        "Export to CSV…",
                        ButtonRole::Secondary,
                        requests,
                        Request::Export,
                    ),
                ),
            ],
        )),
    ])
}

// --- dialogs ------------------------------------------------------------

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
        .on_change(move |value| {
            let mut core = core.borrow_mut();
            let slot = if confirmation {
                &mut core.backup_confirmation
            } else {
                &mut core.backup_password
            };
            *slot = zeroize::Zeroizing::new(value.to_owned());
        });
    let field = Field::new(label, input).with_label_placement(Some(LabelPlacement::Above));
    match error {
        Some(error) => field.with_error(error),
        None if !confirmation => field.with_help("Use at least 12 characters"),
        None => field,
    }
}

fn import_dialog(
    rows: &[crate::transfer::ImportLogin],
    conflicts: usize,
    requests: &Requests,
) -> ModalDialog<Column> {
    let mut children: Vec<Element> = Vec::new();
    if conflicts > 0 {
        children.push(text(
            &if conflicts == 1 {
                "One login is already in your vault and will be skipped.".to_owned()
            } else {
                format!("{conflicts} logins are already in your vault and will be skipped.")
            },
            TextRole::Body,
        ));
    }
    let listed: Vec<Element> = rows
        .iter()
        .take(5)
        .enumerate()
        .map(|(index, row)| {
            Box::new(
                ListRow::new(format!("import:{index}"), row.title.to_string())
                    .icon(Icon::Globe)
                    .secondary(row.username.to_string()),
            ) as Element
        })
        .collect();
    children.push(Box::new(Section::new(listed)));
    if rows.len() > 5 {
        children.push(Box::new(
            Text::new(&format!("And {} more", rows.len().saturating_sub(5)))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    }
    let new = rows.len().saturating_sub(conflicts);
    let title = if new == 1 {
        "Import 1 login?".to_owned()
    } else {
        format!("Import {new} logins?")
    };
    ModalDialog::new(title, body(children))
        .primary_action("Import", ask(requests, Request::ConfirmImport))
        .secondary_action("Cancel", ask(requests, Request::CloseDialog))
}

fn export_dialog(acknowledged: bool, reminded: bool, requests: &Requests) -> ModalDialog<Column> {
    let ack = {
        let requests = Rc::clone(requests);
        Checkbox::from_state(&ui::LocalState::new(acknowledged))
            .with_label("I understand the file is not encrypted")
            .on_change(move |on| requests.borrow_mut().push(Request::Acknowledge(on)))
    };
    let mut children: Vec<Element> = vec![
        text(
            "Anyone who can open the file can read every password in it. Delete it once your passwords are moved.",
            TextRole::Body,
        ),
        Box::new(ack),
    ];
    if reminded {
        children.push(Box::new(
            Text::new("Confirm that you understand before choosing a file.")
                .with_role(TextRole::Caption),
        ));
    }
    ModalDialog::new("Export passwords to a CSV file?", body(children))
        .primary_action("Choose file…", ask(requests, Request::ConfirmExport))
        .secondary_action("Cancel", ask(requests, Request::CloseDialog))
}

fn dialog(core: &Core, shared: &Shared, requests: &Requests) -> Option<Element> {
    let dialog = core.dialog.as_ref()?;
    let close = ask(requests, Request::CloseDialog);
    let built = match dialog {
        Dialog::ImportPreview { rows, conflicts } => import_dialog(rows, *conflicts, requests),
        Dialog::ExportPlaintext {
            acknowledged,
            reminded,
        } => export_dialog(*acknowledged, *reminded, requests),
        Dialog::Backup { error } => {
            let fields: Vec<Element> = vec![
                Box::new(secret_input(shared, false, "Backup password", None)),
                Box::new(secret_input(
                    shared,
                    true,
                    "Confirm backup password",
                    *error,
                )),
            ];
            ModalDialog::new("Back up vault", body(fields))
                .primary_action("Choose file…", ask(requests, Request::ConfirmBackup))
                .secondary_action("Cancel", close)
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
                "The item is removed from your vault and cannot be restored.",
                TextRole::Body,
            )]),
        )
        .with_destructive(true)
        .primary_action("Delete item", ask(requests, Request::ConfirmDelete))
        .secondary_action("Cancel", close),
    };
    Some(Box::new(
        built
            .with_test_id(format!("passwords.dialog.{}", dialog.kind()))
            .on_dismiss(ask(requests, Request::CloseDialog))
            .open(true),
    ))
}
