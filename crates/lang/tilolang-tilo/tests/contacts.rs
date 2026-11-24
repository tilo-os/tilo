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

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
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
    try_call(r, action, args).unwrap_or_else(|why| panic!("`{action}` was refused: {why}"))
}

/// `call`, and what a refusal says rather than a panic.
fn try_call(r: &Replica, action: &str, args: Vec<Value>) -> Result<Value, String> {
    let id = {
        let package = r.host.package(APP).expect("the contacts package");
        action_id(&package.meta, action)
    };
    let who = Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
        r.name.clone(),
    ));
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => Ok(value),
        Ok(other) => Err(format!("{other:?}")),
        Err(error) => Err(error.to_string()),
    }
}

/// The resource an action answered.
fn resource_of(value: Value) -> ResourceRef {
    match value {
        Value::Ref(found) => (*found.target).clone(),
        other => panic!("the action answered no resource: {other:?}"),
    }
}

/// A whole number a field of a record holds.
fn count(r: &Replica, at: &ResourceRef, field: &str) -> i64 {
    match r.read(at, field) {
        Value::Int(number) => number,
        other => panic!("`{field}` of a record: {other:?}"),
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
    let imported = resource_of(call(
        &grace,
        "import_vcard",
        vec![Value::resource(&other), file],
    ));
    grace.commit();
    assert_eq!(count(&grace, &imported, "imported"), 1);
    let read = items(&grace, &other, "contacts");
    assert_eq!(read.len(), 1);
    let read = read[0].clone();
    assert_eq!(text(&grace, &read, "name"), "Ada Lovelace");
    assert_eq!(strings(grace.read(&read, "groups")), ["Work"]);
    let mails = items(&grace, &read, "emails");
    assert_eq!(text(&grace, &mails[0], "value"), "ada@example.org");
    assert_eq!(
        text(&grace, &mails[0], "label"),
        "work",
        "the kind the card said is the kind the mailbox reads as"
    );
}

#[test]
fn a_book_takes_the_cards_of_a_vcard_file_in_one_call() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nORG:US Navy\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Alan Turing\r\nEND:VCARD\r\n";
    let imported = resource_of(call(
        &ada,
        "add_contacts",
        vec![Value::resource(&root), Value::text(cards)],
    ));
    assert_eq!(
        count(&ada, &imported, "imported"),
        2,
        "two cards are two contacts"
    );
    assert_eq!(count(&ada, &imported, "skipped"), 0);
    ada.commit();
    let mut names: Vec<String> = items(&ada, &root, "contacts")
        .iter()
        .map(|c| text(&ada, c, "name"))
        .collect();
    names.sort();
    assert_eq!(names, ["Alan Turing", "Grace Hopper"]);
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

/// A card the workspace's apps do not know yet is shared as a person of its own: the card keeps
/// that person's id, and the directory is told the card's name, numbers and mailboxes, so Mail,
/// Calendar and Messages can reach it.
#[test]
fn a_card_the_apps_do_not_know_is_shared_as_a_person_of_its_own() {
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};

    struct Sent(Rc<RefCell<Vec<Invocation>>>);
    impl Invoker for Sent {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            self.0.borrow_mut().push(invocation.clone());
            Invoked::Done(Value::Unit)
        }
    }

    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    with_mail(&ada, &made, "ada@example.org");
    // A directory that names nobody: this card is no one the apps know.
    let directory = ResourceRef::parse_id(
        "contacts:tilo.contacts.identities",
        "so.tilo.contacts.directory#Identities",
    )
    .expect("the directory's published id");
    ada.host
        .runtime()
        .set_reading(&directory, r#"{"people": []}"#)
        .expect("the directory reads as empty");
    let sent = Rc::new(RefCell::new(Vec::new()));
    ada.host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    call(
        &ada,
        "share_card",
        vec![Value::resource(&directory), Value::resource(&made)],
    );
    ada.commit();

    let person = text(&ada, &made, "person");
    assert!(
        !person.is_empty(),
        "the card keeps the person it was shared as"
    );
    let sent = sent.borrow();
    let project = sent
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action { name, args, .. } if name == "project-contact" => {
                Some(args.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("the card is projected into the directory: {sent:?}"));
    assert!(
        project
            .iter()
            .any(|(name, value)| name == "person" && *value == Value::Text(person.clone())),
        "the person the card keeps is the one the directory is told: {project:?}"
    );
    assert!(
        project.iter().any(
            |(name, value)| name == "label" && *value == Value::Text("Ada Lovelace".to_owned())
        ),
        "the directory is told the card's name: {project:?}"
    );
    let mails = sent
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action { name, args, .. } if name == "project-emails" => Some(args.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the card's mailboxes go with it: {sent:?}"));
    assert!(
        mails.iter().any(|(name, value)| name == "emails"
            && *value == Value::List(vec![Value::Text("ada@example.org".to_owned())])),
        "{mails:?}"
    );
}

/// A file read twice is one book: the card keeps the id its format gave it, and a card the book
/// already holds is nobody new.
#[test]
fn a_book_imported_twice_keeps_one_card() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada-lovelace-1\r\nFN:Ada Lovelace\r\n\
                 EMAIL;TYPE=work:ada@example.org\r\nEND:VCARD\r\n";
    let once = Value::file(device.keeping("Ada.vcf", "text/vcard", cards.as_bytes()));
    let first = resource_of(call(
        &ada,
        "import_vcard",
        vec![Value::resource(&root), once],
    ));
    ada.commit();
    assert_eq!(count(&ada, &first, "imported"), 1);
    let again = Value::file(device.keeping("Ada.vcf", "text/vcard", cards.as_bytes()));
    let second = resource_of(call(
        &ada,
        "import_vcard",
        vec![Value::resource(&root), again],
    ));
    ada.commit();
    assert_eq!(
        count(&ada, &second, "unchanged"),
        1,
        "the file's card is already held"
    );
    assert_eq!(count(&ada, &second, "imported"), 0);
    let held = items(&ada, &root, "contacts");
    assert_eq!(held.len(), 1, "one card, however often its file is read");
    assert_eq!(text(&ada, &held[0], "uid"), "ada-lovelace-1");
}

/// A card whose id the book already holds but which says something else about the person waits
/// for review: the contact the book keeps is not changed, and the person says which card is
/// theirs.
#[test]
fn a_changed_card_waits_for_review_and_the_person_says_which_card_is_theirs() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let card = |org: &str| {
        format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada-lovelace-1\r\nFN:Ada Lovelace\r\n\
             ORG:{org}\r\nEMAIL;TYPE=work:ada@example.org\r\nEND:VCARD\r\n"
        )
    };
    let first = Value::file(device.keeping(
        "Ada.vcf",
        "text/vcard",
        card("Analytical Engines").as_bytes(),
    ));
    let first = resource_of(call(
        &ada,
        "import_vcard",
        vec![Value::resource(&root), first],
    ));
    ada.commit();
    assert_eq!(count(&ada, &first, "imported"), 1);

    // The same id with another organization: nobody is changed before the person says so.
    let changed =
        Value::file(device.keeping("Ada.vcf", "text/vcard", card("Royal Society").as_bytes()));
    let changed = resource_of(call(
        &ada,
        "import_vcard",
        vec![Value::resource(&root), changed],
    ));
    ada.commit();
    assert_eq!(count(&ada, &changed, "imported"), 0);
    assert_eq!(count(&ada, &changed, "awaiting"), 1);
    let held = items(&ada, &root, "contacts");
    assert_eq!(held.len(), 1, "the book keeps what it held");
    assert_eq!(text(&ada, &held[0], "org"), "Analytical Engines");
    let waiting = items(&ada, &root, "review");
    assert_eq!(waiting.len(), 1, "the file's card waits for review");
    assert_eq!(text(&ada, &waiting[0], "name"), "Ada Lovelace");

    // The window offers the place that lists it, counting what waits, and says what the import
    // did until the person answers it.
    let mut window = ada.view(APP);
    window.poll_patches().unwrap();
    let review_place = all(&window, "ui.NavItem")
        .into_iter()
        .find(|place| word(place, "label").as_deref() == Some("To review"))
        .unwrap_or_else(|| panic!("the review place: {:?}", all(&window, "ui.NavItem")));
    assert_eq!(review_place.props.get("live"), Some(&Value::Int(1)));
    let banner = all(&window, "ui.Banner")
        .into_iter()
        .find(|banner| word(banner, "title").as_deref() == Some("No contacts were imported"))
        .unwrap_or_else(|| panic!("the import's outcome: {:?}", all(&window, "ui.Banner")));
    assert!(
        word(&banner, "message")
            .unwrap_or_default()
            .contains("1 card needs review"),
        "{banner:?}"
    );

    // Taking the file's card keeps the book's card and makes the file's a contact of its own.
    let second = resource_of(call(
        &ada,
        "import_as_new",
        vec![Value::resource(&root), Value::resource(&waiting[0])],
    ));
    ada.commit();
    assert!(items(&ada, &root, "review").is_empty());
    let both = items(&ada, &root, "contacts");
    assert_eq!(both.len(), 2, "the file's card is a contact of its own");
    assert_eq!(text(&ada, &second, "org"), "Royal Society");
    assert_ne!(text(&ada, &second, "uid"), "ada-lovelace-1");
    assert_eq!(
        text(&ada, &held[0], "org"),
        "Analytical Engines",
        "the card the book kept keeps everything of its own"
    );
}

/// Leaving the book's card as it is takes the file's card out of the review and changes nothing.
#[test]
fn keeping_the_card_the_book_holds_changes_nothing() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:sam-1\r\nFN:Sam Okafor\r\nORG:A\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nUID:sam-1\r\nFN:Sam Okafor\r\nORG:B\r\nEND:VCARD\r\n";
    let imported = resource_of(call(
        &ada,
        "add_contacts",
        vec![Value::resource(&root), Value::text(cards)],
    ));
    ada.commit();
    assert_eq!(count(&ada, &imported, "imported"), 1);
    assert_eq!(count(&ada, &imported, "awaiting"), 1);
    let waiting = items(&ada, &root, "review");
    assert_eq!(waiting.len(), 1);
    call(
        &ada,
        "keep_existing",
        vec![Value::resource(&root), Value::resource(&waiting[0])],
    );
    ada.commit();
    assert!(items(&ada, &root, "review").is_empty());
    let held = items(&ada, &root, "contacts");
    assert_eq!(held.len(), 1);
    assert_eq!(text(&ada, &held[0], "org"), "A");
}

/// A card that says the same thing in another client's spelling is the card the book holds: no
/// second card, and nothing to review.
#[test]
fn a_card_written_another_way_is_the_card_the_book_holds() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let first = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada-lovelace-1\r\nFN:Ada Lovelace\r\n\
                 ORG:Analytical Engines\r\nEMAIL;TYPE=work:ada@example.org\r\nEND:VCARD\r\n";
    let first = resource_of(call(
        &ada,
        "add_contacts",
        vec![Value::resource(&root), Value::text(first)],
    ));
    ada.commit();
    assert_eq!(count(&ada, &first, "imported"), 1);
    // vCard 3.0, upper-case tags, the format's noise beside the kind, and a folded line.
    let again = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada-lovelace-1\r\nFN:Ada Love\r\n lace\r\n\
                 ORG:Analytical Engines\r\nEMAIL;TYPE=WORK,INTERNET,PREF:ada@example.org\r\nEND:VCARD\r\n";
    let again = resource_of(call(
        &ada,
        "add_contacts",
        vec![Value::resource(&root), Value::text(again)],
    ));
    ada.commit();
    assert_eq!(count(&ada, &again, "unchanged"), 1);
    assert_eq!(count(&ada, &again, "awaiting"), 0);
    assert!(items(&ada, &root, "review").is_empty());
    assert_eq!(items(&ada, &root, "contacts").len(), 1);
}

/// A file beyond what the app imports is refused whole, and the refusal says why.
#[test]
fn an_oversized_file_or_one_with_too_many_cards_is_refused_legibly() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let big = vec![b'a'; 8 * 1024 * 1024 + 1];
    let file = Value::file(device.keeping("big.vcf", "text/vcard", &big));
    let refused = try_call(&ada, "import_vcard", vec![Value::resource(&root), file])
        .expect_err("a file past 8 MB is refused");
    assert!(refused.contains("8 MB"), "{refused}");
    assert!(items(&ada, &root, "contacts").is_empty());

    let many = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:A\r\nEND:VCARD\r\n".repeat(2049);
    let file = Value::file(device.keeping("many.vcf", "text/vcard", many.as_bytes()));
    let refused = try_call(&ada, "import_vcard", vec![Value::resource(&root), file])
        .expect_err("a file of more than 2048 cards is refused");
    assert!(refused.contains("2048"), "{refused}");
    ada.commit();
    assert!(items(&ada, &root, "contacts").is_empty());
}

/// A card that names nobody is counted as one the app could not read, and the rest of the file
/// still comes in.
#[test]
fn a_card_that_names_nobody_is_counted_as_not_read() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nVERSION:4.0\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Alan Turing\r\n";
    let imported = resource_of(call(
        &ada,
        "add_contacts",
        vec![Value::resource(&root), Value::text(cards)],
    ));
    ada.commit();
    assert_eq!(count(&ada, &imported, "imported"), 1);
    assert_eq!(count(&ada, &imported, "skipped"), 2);
    assert_eq!(items(&ada, &root, "contacts").len(), 1);
}

/// The case of a `VCardVersion`.
fn vcard_version(r: &Replica, case: &str) -> Value {
    let package = r.host.package(APP).expect("the contacts package");
    let declared = package
        .meta
        .enums
        .iter()
        .find(|declared| declared.name == "VCardVersion")
        .expect("VCardVersion");
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

/// The day a contact's `birthday` holds, as text, or none.
fn birthday(r: &Replica, c: &ResourceRef) -> Option<String> {
    match r.read(c, "birthday") {
        Value::Opt(None) => None,
        Value::Opt(Some(inner)) => match *inner {
            Value::Text(day) => Some(day),
            other => panic!("a birthday: {other:?}"),
        },
        other => panic!("a birthday: {other:?}"),
    }
}

#[test]
fn a_card_is_written_folded_the_way_the_format_reads() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let long = format!("{}Lovelace", "Ada ".repeat(40));
    let made = contact(&ada, &root, &long);
    with_mail(&ada, &made, "ada@example.org");
    call(
        &ada,
        "add_phone",
        vec![
            Value::resource(&made),
            Value::text("+44 20 7946 0018"),
            Value::text("mobile"),
        ],
    );
    ada.commit();
    let Value::File(three) = call(
        &ada,
        "export_contact",
        vec![Value::resource(&made), vcard_version(&ada, "v3")],
    ) else {
        panic!("export_contact answered no file")
    };
    let written = device.text(&three);
    assert!(written.contains("VERSION:3.0"), "{written}");
    assert!(
        written.contains("TEL;TYPE=MOBILE:+44 20 7946 0018"),
        "3.0 writes a kind in capitals: {written}"
    );
    assert!(
        written.contains("EMAIL;TYPE=WORK:ada@example.org"),
        "{written}"
    );
    assert!(
        written.ends_with("\r\n"),
        "a card ends its last line: {written:?}"
    );
    assert!(
        written.lines().all(|line| line.chars().count() <= 75),
        "every line is within the format's length: {written:?}"
    );
    assert!(
        written.contains("\r\n "),
        "a long line is folded onto the next, which begins with a space: {written:?}"
    );

    // Another workspace reads the folded card as the contact it holds, whole.
    let (grace, other, _) = book(&device);
    let imported = resource_of(call(
        &grace,
        "import_vcard",
        vec![Value::resource(&other), Value::file((*three).clone())],
    ));
    grace.commit();
    assert_eq!(count(&grace, &imported, "imported"), 1);
    let read = items(&grace, &other, "contacts");
    assert_eq!(
        text(&grace, &read[0], "name"),
        long,
        "the folded name reads whole"
    );
    let mails = items(&grace, &read[0], "emails");
    assert_eq!(text(&grace, &mails[0], "label"), "work");

    // The default card is a 4.0 one, with a structured name under its full one.
    let Value::File(four) = call(&ada, "export_book", vec![Value::resource(&root)]) else {
        panic!("export_book answered no file")
    };
    let written = device.text(&four);
    assert!(written.contains("VERSION:4.0"), "{written}");
    assert!(
        written.contains("N:Lovelace;Ada Ada"),
        "the structured name takes the family name after the last space: {written}"
    );
}

/// A birthday a card keeps without its year is kept without one here, and written back without
/// one in either version of the format.
#[test]
fn a_birthday_kept_without_its_year_stays_without_one() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let cards = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada-1\r\nFN:Ada Lovelace\r\nBDAY:--0315\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nUID:grace-1\r\nFN:Grace Hopper\r\n\
                 BDAY;X-APPLE-OMIT-YEAR=1604:1604-0317\r\nEND:VCARD\r\n\
                 BEGIN:VCARD\r\nVERSION:4.0\r\nUID:alan-1\r\nFN:Alan Turing\r\nBDAY:19120623\r\nEND:VCARD\r\n";
    let imported = resource_of(call(
        &ada,
        "add_contacts",
        vec![Value::resource(&root), Value::text(cards)],
    ));
    ada.commit();
    assert_eq!(count(&ada, &imported, "imported"), 3);
    let held = items(&ada, &root, "contacts");
    let of = |name: &str| {
        held.iter()
            .find(|c| text(&ada, c, "name") == name)
            .unwrap_or_else(|| panic!("{name} is a contact: {held:?}"))
            .clone()
    };
    let ada_card = of("Ada Lovelace");
    assert_eq!(birthday(&ada, &ada_card).as_deref(), Some("2000-03-15"));
    assert!(!flag(&ada, &ada_card, "birthday_year"));
    // Apple names a placeholder year for a birthday whose year is unknown.
    let grace_card = of("Grace Hopper");
    assert_eq!(birthday(&ada, &grace_card).as_deref(), Some("2000-03-17"));
    assert!(!flag(&ada, &grace_card, "birthday_year"));
    let alan = of("Alan Turing");
    assert_eq!(birthday(&ada, &alan).as_deref(), Some("1912-06-23"));
    assert!(flag(&ada, &alan, "birthday_year"));

    // The day is written back as the year-less day it is, in the shape each version reads.
    let Value::File(four) = call(
        &ada,
        "export_contact",
        vec![Value::resource(&ada_card), vcard_version(&ada, "v4")],
    ) else {
        panic!("export_contact answered no file")
    };
    assert!(
        device.text(&four).contains("BDAY:--0315"),
        "{}",
        device.text(&four)
    );
    let Value::File(three) = call(
        &ada,
        "export_contact",
        vec![Value::resource(&ada_card), vcard_version(&ada, "v3")],
    ) else {
        panic!("export_contact answered no file")
    };
    assert!(
        device.text(&three).contains("BDAY:--03-15"),
        "{}",
        device.text(&three)
    );
    let Value::File(alan_three) = call(
        &ada,
        "export_contact",
        vec![Value::resource(&alan), vcard_version(&ada, "v3")],
    ) else {
        panic!("export_contact answered no file")
    };
    assert!(
        device.text(&alan_three).contains("BDAY:1912-06-23"),
        "a day with its year keeps it, with the dashes 3.0 writes: {}",
        device.text(&alan_three)
    );
}

/// The window lists the book by name or by where people work, and every place says how many
/// people it holds.
#[test]
fn the_window_sorts_by_organization_and_counts_its_places() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let zoe = contact(&ada, &root, "Ada Lovelace");
    let org = {
        let package = ada.host.package(APP).expect("the contacts package");
        crate::fixtures::field_id(&package.meta, "Contact", "org")
    };
    ada.edit(&zoe, &org, &Value::text("Zeta Labs"));
    let grace = contact(&ada, &root, "Grace Hopper");
    ada.edit(&grace, &org, &Value::text("Alpha Works"));
    call(
        &ada,
        "group",
        vec![
            Value::resource(&root),
            Value::resource(&grace),
            Value::text("Work"),
        ],
    );
    let mut view = ada.view(APP);
    view.poll_patches().unwrap();
    assert_eq!(titles(&rows(&mut view)), ["Ada Lovelace", "Grace Hopper"]);

    // Each place counts the people it holds.
    let places: Vec<(String, Option<i64>)> = all(&view, "ui.NavItem")
        .iter()
        .map(|place| {
            (
                word(place, "label").unwrap_or_default(),
                match place.props.get("count") {
                    Some(Value::Int(count)) => Some(*count),
                    _ => None,
                },
            )
        })
        .collect();
    assert!(
        places.contains(&("All contacts".to_owned(), Some(2))),
        "{places:?}"
    );
    assert!(places.contains(&("Work".to_owned(), Some(1))), "{places:?}");

    // The sort menu offers the orders, and by organization puts the workplaces in order.
    let sort = all(&view, "ui.MenuItem")
        .into_iter()
        .find(|item| word(item, "label").as_deref() == Some("By organization"))
        .unwrap_or_else(|| panic!("the sort menu: {:?}", all(&view, "ui.MenuItem")));
    view.fire(&sort.key, "choose")
        .expect("choosing the order runs");
    view.poll_patches().unwrap();
    assert_eq!(titles(&rows(&mut view)), ["Grace Hopper", "Ada Lovelace"]);
}

/// A number is handed to the app that takes a call or a message as the system's link vocabulary
/// names it, and one with nothing to dial offers neither, saying why.
#[test]
fn a_number_is_handed_to_a_call_or_a_message_and_one_with_nothing_to_dial_is_not() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    call(
        &ada,
        "add_phone",
        vec![
            Value::resource(&made),
            Value::text("+44 (0)20 7946 0018"),
            Value::text("work"),
        ],
    );
    call(
        &ada,
        "add_phone",
        vec![
            Value::resource(&made),
            Value::text("ask Sam"),
            Value::text("home"),
        ],
    );
    ada.commit();
    let mut page = ada.host.open_resource(&made, None).unwrap();
    page.poll_patches().unwrap();
    let links: Vec<(String, Option<String>)> = all(&page, "ui.Link")
        .iter()
        .map(|link| (word(link, "url").unwrap_or_default(), word(link, "label")))
        .collect();
    assert!(
        links.contains(&("tel:+44(0)2079460018".to_owned(), Some("Call".to_owned()))),
        "the number is dialled as its digits: {links:?}"
    );
    assert!(
        links.contains(&("sms:+44(0)2079460018".to_owned(), Some("Text".to_owned()))),
        "{links:?}"
    );
    assert!(
        !links.iter().any(|(url, _)| url.contains("ask")),
        "a value with nothing to dial is no link: {links:?}"
    );
    let notes: Vec<String> = all(&page, "ui.Text")
        .iter()
        .filter_map(|text| word(text, "value"))
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note == "Nothing to dial in this number"),
        "{notes:?}"
    );
}

/// A mailbox is handed to the mail app as a message to it, and one that is not an address is not.
#[test]
fn a_mailbox_is_handed_to_the_mail_app_and_one_that_is_not_an_address_is_not() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    let made = contact(&ada, &root, "Ada Lovelace");
    with_mail(&ada, &made, "ada+lists@example.org");
    with_mail(&ada, &made, "ada lovelace@example.org");
    ada.commit();
    let mut page = ada.host.open_resource(&made, None).unwrap();
    page.poll_patches().unwrap();
    let links: Vec<(String, Option<String>)> = all(&page, "ui.Link")
        .iter()
        .map(|link| (word(link, "url").unwrap_or_default(), word(link, "label")))
        .collect();
    assert!(
        links.contains(&(
            "mailto:ada+lists@example.org".to_owned(),
            Some("Write".to_owned())
        )),
        "{links:?}"
    );
    assert!(
        !links.iter().any(|(url, _)| url.contains("lovelace")),
        "an address a message cannot go to is no link: {links:?}"
    );
    let notes: Vec<String> = all(&page, "ui.Text")
        .iter()
        .filter_map(|text| word(text, "value"))
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note == "Not an address a message can go to"),
        "{notes:?}"
    );
}

/// The cards' text is answered whole, or for the one contact a name calls: what an agent or a
/// script hands on where a person's file chooser cannot.
#[test]
fn the_card_text_is_answered_whole_or_by_name() {
    let device = Device::default();
    let (ada, root, _) = book(&device);
    contact(&ada, &root, "Ada Lovelace");
    contact(&ada, &root, "Grace Hopper");
    let Value::File(one) = call(
        &ada,
        "export_vcard_content",
        vec![Value::resource(&root), Value::text("ada lovelace")],
    ) else {
        panic!("export_vcard_content answered no file")
    };
    let cards = device.text(&one);
    assert!(cards.contains("FN:Ada Lovelace"), "{cards}");
    assert!(!cards.contains("Grace Hopper"), "{cards}");
    let Value::File(whole) = call(&ada, "export_vcard_content", vec![Value::resource(&root)])
    else {
        panic!("export_vcard_content answered no file")
    };
    let cards = device.text(&whole);
    assert!(cards.contains("FN:Ada Lovelace"), "{cards}");
    assert!(cards.contains("FN:Grace Hopper"), "{cards}");
    let refused = try_call(
        &ada,
        "export_vcard_content",
        vec![Value::resource(&root), Value::text("Nobody")],
    )
    .expect_err("a name the book does not keep is refused");
    assert!(refused.contains("Name one contact"), "{refused}");
}
