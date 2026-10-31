//! The selected person, as the content column shows them.
//!
//! The page is ordinary retained widgets: the person's avatar, name and
//! work as a header, their groups as tags, and every detail in one
//! [`PropertyList`] whose labels say what each value is. Phone numbers and
//! addresses copy, the website opens; Edit, Delete and Favorite are the
//! column header's commands, and the call, message and email verbs of the
//! person's own number and address are the window bar's
//! ([`crate::app::ContactsApp::window_toolbar`]).

use ui::TextRole;
use ui::framework::Actions;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::{
    Avatar, AvatarSize, Chip, Column, Element, Property, PropertyList, Row, Scrollable, Text,
};

use crate::actions::ContactsEffect;
use crate::form::Slot;
use crate::model::{Address, Contact};
use crate::resource::Contacts;
use crate::view::Shared;

/// A vCard type ("mobile", "work") as a property label ("Mobile"). A kind
/// another app invented reads as the card wrote it.
pub(crate) fn kind_label(kind: &str) -> String {
    let kind = kind.trim();
    if kind.is_empty() {
        return "Other".into();
    }
    crate::form::TYPE_OPTIONS
        .iter()
        .chain(crate::form::OTHER_KINDS)
        .find(|(value, _)| value.eq_ignore_ascii_case(kind))
        .map_or_else(|| kind.to_owned(), |(_, label)| (*label).to_owned())
}

/// A kind with what it qualifies ("Work email", "Home address"), so facts
/// of different sorts read apart in one list. A kind that already names
/// the sort ("Email") stands alone.
fn qualified(kind: &str, noun: &str) -> String {
    let label = kind_label(kind);
    if label.eq_ignore_ascii_case(noun) {
        label
    } else {
        format!("{label} {noun}")
    }
}

/// A phone number's label: the kinds that are themselves a sort of line
/// ("Mobile", "Fax", "Pager") stand alone, the rest say "phone".
fn phone_label(kind: &str) -> String {
    let label = kind_label(kind);
    match label.as_str() {
        "Mobile" | "Fax" | "Pager" | "Phone" | "Textphone" => label,
        _ => format!("{label} phone"),
    }
}

/// A postal address as the lines an envelope shows.
pub(crate) fn address_lines(address: &Address) -> String {
    let locality = [
        address.city.trim(),
        address.region.trim(),
        address.postal.trim(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    [
        address.street.trim(),
        locality.as_str(),
        address.country.trim(),
    ]
    .into_iter()
    .filter(|line| !line.is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

/// The person: avatar, name, and what they do and where.
fn header(contact: &Contact, shared: &Shared) -> Element {
    let name = contact.display_name();
    let identity = contact.id.to_string();
    let mut avatar = Avatar::person(&name, identity).size(AvatarSize::Hero);
    {
        let bridge = shared.borrow();
        if let Some(images) = &bridge.images {
            avatar =
                avatar.with_image_url(images.clone(), bridge.pictures.hold_text(&contact.photo));
        }
    }

    let mut who = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new(&name)
                .with_role(TextRole::Title)
                .with_heading_level(1),
        ));
    let work = [contact.title.trim(), contact.org.trim()]
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !work.is_empty() {
        who = who.push(Box::new(
            Text::new(&work).with_role(TextRole::Body).dimmed(),
        ));
    }
    Box::new(
        Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(avatar))
            .push(Box::new(who)),
    )
}

/// The number a call or a message from this page goes to: the first number
/// the person has, which is the one the card and the list show first.
/// `None` when they have no number, so the bar offers no call.
pub(crate) fn primary_phone(contact: &Contact) -> Option<&str> {
    contact
        .phones
        .iter()
        .map(|phone| phone.value.trim())
        .find(|number| crate::handoff::tel_uri(number).is_some())
}

/// The address an email from this page goes to: the first the person has.
/// `None` when they have none, so the bar offers no email.
pub(crate) fn primary_email(contact: &Contact) -> Option<&str> {
    contact
        .emails
        .iter()
        .map(|email| email.value.trim())
        .find(|address| crate::handoff::mailto_uri(address).is_some())
}

/// The page for `contact`.
/// `scroll` keeps the page where it was when the page is rebuilt for the
/// same person.
pub(crate) fn contact_page(
    contact: &Contact,
    shared: &Shared,
    actions: Option<&Actions<Contacts>>,
    scroll: &std::rc::Rc<std::cell::Cell<(f32, f32)>>,
) -> Element {
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_id("contacts.detail")
        .push(header(contact, shared));

    let groups: Vec<&str> = contact
        .groups
        .iter()
        .map(|group| group.value.trim())
        .filter(|group| !group.is_empty())
        .collect();
    if !groups.is_empty() {
        let mut tags = Row::new();
        for name in groups {
            tags = tags.push(Box::new(Chip::new(name)));
        }
        page = page.push(Box::new(tags));
    }

    let facts = contact_properties(contact, actions);
    if !facts.is_empty() {
        page = page.push(Box::new(PropertyList::new(facts)));
    }

    Box::new(
        Scrollable::new_vertical()
            .with_semantic_key("contacts.detail")
            .with_persisted_offset(std::rc::Rc::clone(scroll))
            // The facts keep to the readable column on the leading edge,
            // the page gutter around it.
            .with_child(Box::new(
                ui::widget::Container::new()
                    .with_inset(ui::tokens::Rhythm::Group)
                    .readable()
                    .with_child(Box::new(page.with_no_padding())),
            )),
    )
}

/// `property` with a trailing button running `effect`, when the book
/// publishes it.
fn with_effect(
    property: Property,
    actions: Option<&Actions<Contacts>>,
    icon: Icon,
    label: &str,
    effect: &ContactsEffect,
) -> Property {
    match actions.and_then(|actions| actions.effect(effect)) {
        Some(binding) => property.action_binding(icon, label, binding),
        None => property,
    }
}

/// `property` with an Edit button opening the contact's form on `slot`.
fn editable(
    property: Property,
    actions: Option<&Actions<Contacts>>,
    contact: &Contact,
    slot: Slot,
) -> Property {
    let Some(field) = slot.detail_key() else {
        return property;
    };
    with_effect(
        property,
        actions,
        Icon::Pencil,
        "Edit",
        &ContactsEffect::EditField {
            contact: contact.id,
            field,
        },
    )
}

/// Editable contact facts in their shared label/value column, in reading order.
fn contact_properties(contact: &Contact, actions: Option<&Actions<Contacts>>) -> Vec<Property> {
    // Every fact in one list, so its labels and values keep one column down
    // the page; the label says what kind of fact a value is.
    let mut facts: Vec<Property> = Vec::new();
    facts.extend(
        contact
            .phones
            .iter()
            .enumerate()
            .filter(|(_, phone)| !phone.value.trim().is_empty())
            .map(|(index, phone)| {
                let property = Property::new(phone_label(&phone.label), phone.value.clone())
                    .with_kind(TextKind::Numeric)
                    .copyable();
                editable(property, actions, contact, Slot::Phone(index))
            }),
    );
    facts.extend(
        contact
            .emails
            .iter()
            .enumerate()
            .filter(|(_, email)| !email.value.trim().is_empty())
            .map(|(index, email)| {
                let property = Property::new(qualified(&email.label, "email"), email.value.clone())
                    .with_kind(TextKind::Identifier)
                    .copyable();
                editable(property, actions, contact, Slot::Email(index))
            }),
    );
    facts.extend(
        contact
            .addresses
            .iter()
            .enumerate()
            .filter(|(_, address)| !address.is_empty())
            .map(|(index, address)| {
                let property =
                    Property::new(qualified(&address.label, "address"), address_lines(address))
                        .copyable();
                editable(property, actions, contact, Slot::AddrStreet(index))
            }),
    );
    if let Some(birthday) = &contact.birthday {
        let property = Property::new("Birthday", birthday.pretty()).with_kind(TextKind::Numeric);
        facts.push(editable(property, actions, contact, Slot::Birthday));
    }
    let website = contact.website.trim();
    if !website.is_empty() {
        let property = Property::new("Website", website).with_kind(TextKind::Identifier);
        facts.push(with_effect(
            property,
            actions,
            Icon::ExternalLink,
            "Open",
            &ContactsEffect::OpenWebsite {
                contact: contact.id,
            },
        ));
    }
    let notes = contact.notes.trim();
    if !notes.is_empty() {
        facts.push(editable(
            Property::new("Notes", notes),
            actions,
            contact,
            Slot::Notes,
        ));
    }
    facts
}
