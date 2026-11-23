//! The Contacts app (`apps/tl/contacts`): a card made, named and starred; its numbers, mailboxes,
//! addresses and groups; the two cards that are one person merged into one; the book and a card
//! written as vCards and read back, pictures and all; the person a mailbox belongs to; and what
//! the window and the card's page draw of them.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::value::EnumValue;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Package;

use crate::fixtures::{Replica, Workspace, action_id};

const CONTACTS: (&str, &str) = (
    concat!(
        include_str!("../../../../apps/tl/contacts/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/contacts/identities.tl")
    ),
    include_str!("../../../../apps/tl/contacts/app.ids"),
);
const APP: &str = "so.tilo.contacts";

/// The device the test runs on: the files it was handed, as a content store keeps them.
#[derive(Clone, Default)]
struct Device {
    content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
}

impl Device {
    fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
        self.keep(name, media, bytes).expect("the bytes are kept")
    }

    fn text(&self, file: &FileValue) -> String {
        let held = self.content.borrow();
        String::from_utf8(held.get(&file.hash).cloned().unwrap_or_default()).unwrap()
    }
}

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
        Ok(None)
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
        self.content
            .borrow_mut()
            .insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// Ada's workspace with an empty book, on `device` for its files.
///
/// The app is compiled with the platform packages in hand, as the session has them: `associations`
/// reads the workspace's contact directory by its named root, and its accounts are the directory's
/// verbs.
fn book(device: &Device) -> (Replica, ResourceRef, Vec<Package>) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[]);
    let metas = tilolang_tilo::platform_apps::platform_packages().expect("the platform packages");
    let directory = metas
        .iter()
        .find(|meta| meta.app == "so.tilo.contacts.directory")
        .expect("the contact directory's package")
        .clone();
    ada.host
        .runtime()
        .load_foreign(directory)
        .expect("the directory is a package of the workspace");
    let lock = tilolang::IdsLock::parse(CONTACTS.1).expect("the app's ids lock");
    let compiled = tilolang::compile_package(CONTACTS.0, &lock, &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let package = ada.host.load_compiled(&compiled).expect("the app loads");
    ada.host.runtime().set_blobs(device.clone());
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    (ada, root, vec![package])
}

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let id = {
        let package = r.host.package(APP).expect("the contacts package");
        action_id(&package.meta, action)
    };
    let who = Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
        r.name.clone(),
    ));
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

fn text(r: &Replica, at: &ResourceRef, field: &str) -> String {
    match r.read(at, field) {
        Value::Text(text) => text,
        other => panic!("`{field}` of a contact: {other:?}"),
    }
}

fn flag(r: &Replica, at: &ResourceRef, field: &str) -> bool {
    match r.read(at, field) {
        Value::Bool(on) => on,
        other => panic!("`{field}` of a contact: {other:?}"),
    }
}

fn items(r: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match r.read(at, field) {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(r) => Some((*r.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("`{field}` of a contact: {other:?}"),
    }
}

fn strings(value: Value) -> Vec<String> {
    match value {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Text(text) => Some(text),
                _ => None,
            })
            .collect(),
        other => panic!("a set of words: {other:?}"),
    }
}

fn groups(r: &Replica, c: &ResourceRef) -> Vec<String> {
    strings(r.read(c, "groups"))
}

/// A new contact named `name`, as the book's entry makes one.
fn contact(r: &Replica, root: &ResourceRef, name: &str) -> ResourceRef {
    let made = call(
        r,
        "new_contact",
        vec![Value::resource(root), Value::text(name)],
    );
    r.commit();
    match made {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("new_contact answered {other:?}"),
    }
}

/// A contact given a mailbox, as an agent or another app adds one.
fn with_mail(r: &Replica, c: &ResourceRef, address: &str) -> ResourceRef {
    let made = call(
        r,
        "add_email",
        vec![
            Value::resource(c),
            Value::text(address),
            Value::text("work"),
        ],
    );
    r.commit();
    match made {
        Value::Ref(mail) => (*mail.target).clone(),
        other => panic!("add_email answered {other:?}"),
    }
}

/// The case a `MergeField` enum parameter takes.
fn merge_field(r: &Replica, case: &str) -> Value {
    let package = r.host.package(APP).expect("the contacts package");
    let declared = package
        .meta
        .enums
        .iter()
        .find(|declared| declared.name == "MergeField")
        .expect("MergeField");
    let variant = declared
        .variants
        .iter()
        .find(|variant| variant.name == case)
        .expect("the case");
    Value::Enum(EnumValue {
        ty: declared.id.clone(),
        variant: variant.id.clone(),
        fields: BTreeMap::new(),
    })
}

/// A word a node's prop holds, whether the adapter handed it over as a word or as the option of
/// one.
fn word(node: &UiNode, prop: &str) -> Option<String> {
    match node.props.get(prop) {
        Some(Value::Text(text)) => Some(text.clone()),
        Some(Value::Opt(Some(inner))) => match &**inner {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Every node of `kind`, anywhere in the view.
fn all(view: &ViewInstance, kind: &str) -> Vec<UiNode> {
    view.tree().of_kind(kind).into_iter().cloned().collect()
}

/// The rows the window's list builds, each as the row node the person reads.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let Some(list) = view
        .tree()
        .first_of_kind("ui.VirtualList")
        .map(|list| list.key.clone())
    else {
        return Vec::new();
    };
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

fn titles(rows: &[UiNode]) -> Vec<String> {
    rows.iter()
        .flat_map(|row| row.of_kind("ui.ListRow"))
        .filter_map(|row| word(row, "title"))
        .collect()
}

#[test]
fn a_contact_is_made_named_and_listed() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    assert_eq!(text(&ada, &made, "name"), "Ada Lovelace");
    let listed = items(&ada, &root, "contacts");
    assert_eq!(listed.len(), 1, "{listed:?}");
    // The window lists the card under its own name.
    let mut window = ada.view(APP);
    assert_eq!(titles(&rows(&mut window)), ["Ada Lovelace"]);
    // A card made with no name is called by the words its type gives one.
    let unnamed = call(
        &ada,
        "new_contact",
        vec![Value::resource(&root), Value::text("")],
    );
    ada.commit();
    let Value::Ref(unnamed) = unnamed else {
        panic!("new_contact answered no contact")
    };
    assert_eq!(text(&ada, unnamed.target.as_ref(), "name"), "");
}

#[test]
fn a_contact_is_renamed_and_starred() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada");
    call(
        &ada,
        "rename",
        vec![Value::resource(&made), Value::text("Ada Lovelace")],
    );
    ada.commit();
    assert_eq!(text(&ada, &made, "name"), "Ada Lovelace");
    call(&ada, "favorite", vec![Value::resource(&made)]);
    ada.commit();
    assert!(flag(&ada, &made, "favorite"));
    // The row shows the star the mark names.
    let mut window = ada.view(APP);
    let listed = rows(&mut window);
    let marks: Vec<Option<String>> = listed
        .iter()
        .flat_map(|row| row.of_kind("ui.ListRow"))
        .map(|row| word(row, "mark"))
        .collect();
    assert!(
        marks.iter().any(|mark| mark.as_deref() == Some("star")),
        "{marks:?}"
    );
}

#[test]
fn numbers_mailboxes_and_addresses_are_added_and_taken_away() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Grace Hopper");
    let Value::Ref(phone) = call(
        &ada,
        "add_phone",
        vec![
            Value::resource(&made),
            Value::text("+44 20 7946 0018"),
            Value::text("mobile"),
        ],
    ) else {
        panic!("add_phone answered no row")
    };
    let phone = (*phone.target).clone();
    ada.commit();
    assert_eq!(items(&ada, &made, "phones").len(), 1);
    assert_eq!(text(&ada, &phone, "value"), "+44 20 7946 0018");
    assert_eq!(text(&ada, &phone, "label"), "mobile");
    call(
        &ada,
        "remove_phone",
        vec![Value::resource(&made), Value::resource(&phone)],
    );
    ada.commit();
    assert!(items(&ada, &made, "phones").is_empty());

    let mail = with_mail(&ada, &made, "grace@example.org");
    assert_eq!(text(&ada, &mail, "value"), "grace@example.org");
    call(
        &ada,
        "remove_email",
        vec![Value::resource(&made), Value::resource(&mail)],
    );
    ada.commit();
    assert!(items(&ada, &made, "emails").is_empty());

    let Value::Ref(address) = call(&ada, "add_address", vec![Value::resource(&made)]) else {
        panic!("add_address answered no row")
    };
    let address = (*address.target).clone();
    ada.commit();
    assert_eq!(items(&ada, &made, "addresses").len(), 1);
    call(
        &ada,
        "remove_address",
        vec![Value::resource(&made), Value::resource(&address)],
    );
    ada.commit();
    assert!(items(&ada, &made, "addresses").is_empty());
}

#[test]
fn a_group_makes_a_place_and_a_chip() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    call(
        &ada,
        "group",
        vec![
            Value::resource(&root),
            Value::resource(&made),
            Value::text("Work"),
        ],
    );
    ada.commit();
    assert_eq!(groups(&ada, &made), ["Work"]);
    assert_eq!(items(&ada, &root, "groups").len(), 1);
    // The sidebar offers the group the book keeps as a place.
    let mut window = ada.view(APP);
    window.poll_patches().unwrap();
    let places: Vec<String> = all(&window, "ui.NavItem")
        .iter()
        .filter_map(|place| word(place, "label"))
        .collect();
    assert!(places.iter().any(|place| place == "Work"), "{places:?}");
    assert!(
        places.iter().any(|place| place == "All contacts"),
        "{places:?}"
    );
    call(
        &ada,
        "ungroup",
        vec![Value::resource(&made), Value::text("Work")],
    );
    ada.commit();
    assert!(groups(&ada, &made).is_empty());
}

#[test]
fn a_contact_is_deleted() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Sam Okafor");
    call(
        &ada,
        "delete",
        vec![Value::resource(&root), Value::resource(&made)],
    );
    ada.commit();
    assert!(items(&ada, &root, "contacts").is_empty());
}

#[test]
fn two_cards_that_share_a_mailbox_are_one_person_and_merge_keeps_everything() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let keep = contact(&ada, &root, "Ada Lovelace");
    with_mail(&ada, &keep, "ADA@example.org");
    let goes = contact(&ada, &root, "A. Lovelace");
    with_mail(&ada, &goes, "ada@example.org");
    let Value::Ref(phone) = call(
        &ada,
        "add_phone",
        vec![
            Value::resource(&goes),
            Value::text("+44 20 7946 0099"),
            Value::text("work"),
        ],
    ) else {
        panic!("add_phone answered no row")
    };
    let phone = (*phone.target).clone();
    ada.commit();
    let number = text(&ada, &phone, "value");
    // The card's page offers the other card as the same person, and what they share is why.
    let mut page = ada.host.open_resource(&keep, None).unwrap();
    page.poll_patches().unwrap();
    let sections: Vec<String> = all(&page, "ui.Section")
        .iter()
        .filter_map(|section| word(section, "title"))
        .collect();
    assert!(
        sections
            .iter()
            .any(|title| title == "Possibly the same person"),
        "{sections:?}"
    );
    // Merging takes the fields asked for and keeps every number of both.
    call(
        &ada,
        "merge_into",
        vec![
            Value::resource(&root),
            Value::resource(&keep),
            Value::resource(&goes),
            Value::List(vec![merge_field(&ada, "name")]),
        ],
    );
    ada.commit();
    assert_eq!(text(&ada, &keep, "name"), "A. Lovelace");
    assert_eq!(items(&ada, &root, "contacts").len(), 1);
    let mails = items(&ada, &keep, "emails");
    assert_eq!(mails.len(), 2, "both mailboxes are the card that stays");
    let phones = items(&ada, &keep, "phones");
    assert_eq!(
        phones.len(),
        1,
        "the other card's number is the kept card's too"
    );
    assert_eq!(
        text(&ada, &phones[0], "value"),
        number,
        "with the number the other card held"
    );
}

#[test]
fn a_book_written_as_vcards_reads_back_with_every_field() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    with_mail(&ada, &made, "ada@example.org");
    call(
        &ada,
        "group",
        vec![
            Value::resource(&root),
            Value::resource(&made),
            Value::text("Work"),
        ],
    );
    ada.commit();
    let Value::File(file) = call(&ada, "export_book", vec![Value::resource(&root)]) else {
        panic!("export_book answered no file")
    };
    let cards = device.text(&file);
    assert!(cards.contains("BEGIN:VCARD"), "{cards}");
    assert!(cards.contains("FN:Ada Lovelace"), "{cards}");
    assert!(cards.contains("EMAIL;TYPE=work:ada@example.org"), "{cards}");
    assert!(cards.contains("CATEGORIES:Work"), "{cards}");

    // Another workspace reads the file back as the card it holds.
    let (grace, other, _) = book(&device);
    let file = Value::file(device.keeping("Contacts.vcf", "text/vcard", cards.as_bytes()));
    let imported = call(&grace, "import_vcard", vec![Value::resource(&other), file]);
    grace.commit();
    assert!(matches!(imported, Value::Int(1)), "{imported:?}");
    let read = items(&grace, &other, "contacts");
    assert_eq!(read.len(), 1);
    let read = read[0].clone();
    assert_eq!(text(&grace, &read, "name"), "Ada Lovelace");
    assert_eq!(strings(grace.read(&read, "groups")), ["Work"]);
    let mails = items(&grace, &read, "emails");
    assert_eq!(text(&grace, &mails[0], "value"), "ada@example.org");
}

#[test]
fn a_photo_travels_as_the_address_the_avatar_shows() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\n\
                PHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\nEND:VCARD\r\n";
    let file = Value::file(device.keeping("Ada.vcf", "text/vcard", card.as_bytes()));
    call(&ada, "import_vcard", vec![Value::resource(&root), file]);
    ada.commit();
    let read = items(&ada, &root, "contacts");
    let photo = text(&ada, &read[0], "photo");
    assert!(
        photo.starts_with("tilo-blob://") && !photo.contains("base64"),
        "a portrait is the address of the content the device keeps, not its bytes: {photo}"
    );
    // The bytes were kept once, under that address.
    assert!(
        device
            .content
            .borrow()
            .contains_key(photo.trim_start_matches("tilo-blob://")),
        "the device keeps the portrait's bytes: {photo}"
    );
    // The card's avatar is given the picture, which it draws instead of the initials.
    let mut page = ada.host.open_resource(&read[0], None).unwrap();
    page.poll_patches().unwrap();
    let pictures: Vec<Option<String>> = all(&page, "ui.Avatar")
        .iter()
        .map(|avatar| word(avatar, "picture"))
        .collect();
    assert!(
        pictures
            .iter()
            .any(|url| url.as_deref() == Some(photo.as_str())),
        "{pictures:?}"
    );
    // And it is written back as the PHOTO line a reader of vCards takes.
    let Value::File(file) = call(&ada, "export_contact", vec![Value::resource(&read[0])]) else {
        panic!("export_contact answered no file")
    };
    let written = device.text(&file);
    assert!(
        written.contains(&format!("PHOTO;VALUE=uri:{photo}")),
        "{written}"
    );
}

#[test]
fn the_person_a_mailbox_belongs_to_is_found() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    with_mail(&ada, &made, "Ada@Example.org");
    let found = call(
        &ada,
        "find_for_address",
        vec![Value::resource(&root), Value::text("ada@example.org")],
    );
    let Value::Opt(Some(found)) = found else {
        panic!("the mailbox finds its contact: {found:?}")
    };
    let Value::Ref(found) = *found else {
        panic!("the answer is the contact: {found:?}")
    };
    assert_eq!(found.target.id_string(), made.id_string());
    let nobody = call(
        &ada,
        "find_for_address",
        vec![Value::resource(&root), Value::text("nobody@example.org")],
    );
    assert!(matches!(nobody, Value::Opt(None)), "{nobody:?}");
}

#[test]
fn an_empty_book_says_so_and_the_entry_makes_the_first_card() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let mut window = ada.view(APP);
    window.poll_patches().unwrap();
    let empty: Vec<String> = all(&window, "ui.EmptyState")
        .iter()
        .filter_map(|state| word(state, "title"))
        .collect();
    assert!(
        empty.iter().any(|title| title == "No contacts yet"),
        "{empty:?}"
    );
    // The card's page writes what is bound to it: the name, an organization and a note.
    let made = contact(&ada, &root, "Ada Lovelace");
    let mut page = ada.host.open_resource(&made, None).unwrap();
    page.poll_patches().unwrap();
    let fields: Vec<String> = all(&page, "ui.TextInput")
        .iter()
        .filter_map(|field| word(field, "placeholder"))
        .collect();
    assert!(
        fields.iter().any(|field| field == "Where they work"),
        "{fields:?}"
    );
    let rich = all(&page, "ui.BlockEditor");
    assert!(!rich.is_empty(), "the card keeps notes");
    let _note: RichValue = match ada.read(&made, "notes") {
        Value::Rich(note) => note,
        other => panic!("a card's notes: {other:?}"),
    };
}
