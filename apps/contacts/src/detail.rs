//! The selected person, as the content column shows them.
//!
//! The page is ordinary retained widgets: the person's avatar, name and
//! work as a header, their groups as tags, and every detail in one
//! [`PropertyList`] whose labels say what each value is. Phone numbers and
//! addresses copy, the website opens; Edit, Delete and Favorite are the
//! column header's commands.

use ui::TextRole;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::{
    Avatar, AvatarSize, Chip, Column, Element, Property, PropertyList, Row, Scrollable, Text,
};

use crate::form::Slot;
use crate::model::{Address, Contact};
use crate::view::{Request, Shared, queue};

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
fn header(contact: &Contact) -> Element {
    let name = contact.display_name();
    let identity = contact.id.to_string();

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
            .push(Box::new(
                Avatar::person(name, identity).size(AvatarSize::Hero),
            ))
            .push(Box::new(who)),
    )
}

/// The page for `contact`.
/// `scroll` keeps the page where it was when the page is rebuilt for the
/// same person.
pub(crate) fn contact_page(
    contact: &Contact,
    shared: &Shared,
    scroll: &std::rc::Rc<std::cell::Cell<(f32, f32)>>,
) -> Element {
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_id("contacts.detail")
        .push(header(contact));

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

    let facts = contact_properties(contact, shared);
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

/// Editable contact facts in their shared label/value column, in reading order.
fn contact_properties(contact: &Contact, shared: &Shared) -> Vec<Property> {
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
                Property::new(phone_label(&phone.label), phone.value.clone())
                    .with_kind(TextKind::Numeric)
                    .copyable()
                    .action(
                        Icon::Pencil,
                        "Edit",
                        queue(shared, Request::EditField(contact.id, Slot::Phone(index))),
                    )
            }),
    );
    facts.extend(
        contact
            .emails
            .iter()
            .enumerate()
            .filter(|(_, email)| !email.value.trim().is_empty())
            .map(|(index, email)| {
                Property::new(qualified(&email.label, "email"), email.value.clone())
                    .with_kind(TextKind::Identifier)
                    .copyable()
                    .action(
                        Icon::Pencil,
                        "Edit",
                        queue(shared, Request::EditField(contact.id, Slot::Email(index))),
                    )
            }),
    );
    facts.extend(
        contact
            .addresses
            .iter()
            .enumerate()
            .filter(|(_, address)| !address.is_empty())
            .map(|(index, address)| {
                Property::new(qualified(&address.label, "address"), address_lines(address))
                    .copyable()
                    .action(
                        Icon::Pencil,
                        "Edit",
                        queue(
                            shared,
                            Request::EditField(contact.id, Slot::AddrStreet(index)),
                        ),
                    )
            }),
    );
    if let Some(birthday) = &contact.birthday {
        facts.push(
            Property::new("Birthday", birthday.pretty())
                .with_kind(TextKind::Numeric)
                .action(
                    Icon::Pencil,
                    "Edit",
                    queue(shared, Request::EditField(contact.id, Slot::Birthday)),
                ),
        );
    }
    let website = contact.website.trim();
    if !website.is_empty() {
        facts.push(
            Property::new("Website", website)
                .with_kind(TextKind::Identifier)
                .action(
                    Icon::ExternalLink,
                    "Open",
                    queue(shared, Request::OpenWebsite(website.to_owned())),
                ),
        );
    }
    let notes = contact.notes.trim();
    if !notes.is_empty() {
        facts.push(Property::new("Notes", notes).action(
            Icon::Pencil,
            "Edit",
            queue(shared, Request::EditField(contact.id, Slot::Notes)),
        ));
    }
    facts
}
