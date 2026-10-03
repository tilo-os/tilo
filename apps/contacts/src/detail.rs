//! The selected person, as the content column shows them.
//!
//! The page is ordinary retained widgets: the person's avatar, name and
//! work as a header, their groups as tags, and every detail as a
//! [`PropertyList`] under a sentence-case group title. Phone numbers and
//! addresses copy, the website opens; Edit, Delete and Favorite are the
//! column header's commands.

use ui::TextRole;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::{
    Avatar, AvatarSize, Chip, Column, Element, Property, PropertyList, Row, Scrollable, Section,
    SectionHeader, Text,
};

use crate::model::{Address, Contact};
use crate::view::{Readable, Request, Shared, queue};

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

fn group(title: &str, body: Element) -> Element {
    Box::new(Section::new(vec![body]).with_header(SectionHeader::group(title)))
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

    let phones: Vec<Property> = contact
        .phones
        .iter()
        .filter(|phone| !phone.value.trim().is_empty())
        .map(|phone| {
            Property::new(kind_label(&phone.label), phone.value.clone())
                .with_kind(TextKind::Numeric)
                .copyable()
        })
        .collect();
    if !phones.is_empty() {
        page = page.push(group("Phone", Box::new(PropertyList::new(phones))));
    }

    let emails: Vec<Property> = contact
        .emails
        .iter()
        .filter(|email| !email.value.trim().is_empty())
        .map(|email| {
            Property::new(kind_label(&email.label), email.value.clone())
                .with_kind(TextKind::Identifier)
                .copyable()
        })
        .collect();
    if !emails.is_empty() {
        page = page.push(group("Email", Box::new(PropertyList::new(emails))));
    }

    let addresses: Vec<Property> = contact
        .addresses
        .iter()
        .filter(|address| !address.is_empty())
        .map(|address| Property::new(kind_label(&address.label), address_lines(address)).copyable())
        .collect();
    if !addresses.is_empty() {
        page = page.push(group("Address", Box::new(PropertyList::new(addresses))));
    }

    let mut details = Vec::new();
    if let Some(birthday) = &contact.birthday {
        details.push(Property::new("Birthday", birthday.pretty()).with_kind(TextKind::Numeric));
    }
    let website = contact.website.trim();
    if !website.is_empty() {
        details.push(
            Property::new("Website", website)
                .with_kind(TextKind::Identifier)
                .action(
                    Icon::ExternalLink,
                    "Open website",
                    queue(shared, Request::OpenWebsite(website.to_owned())),
                ),
        );
    }
    if !details.is_empty() {
        page = page.push(group("Details", Box::new(PropertyList::new(details))));
    }

    let notes = contact.notes.trim();
    if !notes.is_empty() {
        page = page.push(group(
            "Notes",
            Box::new(Text::new(notes).with_role(TextRole::Body)),
        ));
    }

    Box::new(
        Scrollable::new_vertical()
            .with_semantic_key("contacts.detail")
            .with_persisted_offset(std::rc::Rc::clone(scroll))
            .with_child(Box::new(Readable::new(page))),
    )
}
