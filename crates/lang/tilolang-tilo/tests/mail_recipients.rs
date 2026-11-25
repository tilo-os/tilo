//! Mail's recipient fields offer the workspace's address book as a name or an address is typed:
//! the tilolang Mail reads the Contacts app's book, and taking a suggestion writes the person's
//! structured address (`Ada Lovelace <ada@example.org>`) after the recipients already typed.

use std::path::Path;

use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, Value, ViewInstance};
use tilolang_tilo::host::Host;

use crate::fixtures::{Replica, Workspace, field_id, target};

const CONTACTS: &str = "so.tilo.contacts";
const MAIL: &str = "so.tilo.mail";

/// The package of `apps/tl/<dir>` as it ships, compiled against the packages it uses.
fn compiled(dir: &str, metas: &[tilolang::PackageMeta]) -> tilolang::Compiled {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/tl")
        .join(dir);
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    tilolang::compile_package_source(&source, &ids, metas)
        .unwrap_or_else(|diagnostics| panic!("{}: {diagnostics:#?}", dir.display()))
}

fn call(r: &Replica, app: &str, action: &str, args: Vec<Value>) -> Value {
    let id = {
        let package = r.host.package(app).expect("the package");
        let named: Vec<_> = package
            .meta
            .actions
            .iter()
            .filter(|declared| declared.name == action)
            .collect();
        // A name the mailbox's root shares with a type's (`open_link` on a message, and the
        // mailbox's that a `mailto:` link reaches) is the root's here: the test calls the app
        // on its own root's actions.
        let root = package
            .meta
            .types
            .iter()
            .find(|ty| package.meta.roots.contains(&ty.id))
            .map(|ty| ty.name.as_str());
        named
            .iter()
            .find(|declared| {
                declared
                    .params
                    .first()
                    .is_some_and(|param| Some(param.ty.as_str()) == root)
            })
            .or_else(|| named.first())
            .unwrap_or_else(|| panic!("the action `{action}`"))
            .id
            .clone()
    };
    let who = Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
        r.name.clone(),
    ));
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// Ada's replica with Mail and Contacts loaded, her book knowing Anna Kowalski, and a draft of
/// hers that names nobody yet.
fn mailbox() -> (Replica, ResourceRef) {
    let workspace = Workspace::new();
    let host = Host::open(
        workspace.store(),
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        None,
        || None,
    )
    .expect("a host");
    let platform =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform packages");
    let directory = platform
        .iter()
        .find(|meta| meta.app == tilolang_tilo::platform_apps::CONTACTS_APP)
        .expect("the contact directory's package")
        .clone();
    host.runtime()
        .load_foreign(directory)
        .expect("the directory is a package of the workspace");
    let contacts = compiled("contacts", &platform);
    host.load_compiled(&contacts)
        .expect("the contacts package loads");
    let mut metas = platform;
    metas.push(contacts.meta);
    let mail = compiled("mail", &metas);
    host.load_compiled(&mail).expect("the mail package loads");
    let book = host.root_of(CONTACTS).expect("the contacts' book");
    let mail_root = host.root_of(MAIL).expect("the mail root");
    let ada = Replica::with_host("ada", workspace, host);
    let anna = target(&call(
        &ada,
        CONTACTS,
        "new_contact",
        vec![Value::resource(&book), Value::text("Anna Kowalski")],
    ));
    call(
        &ada,
        CONTACTS,
        "add_email",
        vec![
            Value::resource(&anna),
            Value::text("anna@northwind.example"),
            Value::text("work"),
        ],
    );
    let bo = target(&call(
        &ada,
        CONTACTS,
        "new_contact",
        vec![Value::resource(&book), Value::text("Bo Diaz")],
    ));
    call(
        &ada,
        CONTACTS,
        "add_email",
        vec![
            Value::resource(&bo),
            Value::text("bo@boats.example"),
            Value::text("work"),
        ],
    );
    let draft = target(&call(
        &ada,
        MAIL,
        "new_draft",
        vec![Value::resource(&mail_root)],
    ));
    (ada, draft)
}

/// What the draft's recipient field offers under it, as (what taking it writes, its row, the
/// address under it).
fn offers(view: &ViewInstance) -> Vec<(String, String, String)> {
    let text = |row: &tilolang_runtime::UiNode, prop: &str| match row.props.get(prop) {
        Some(Value::Text(text)) => text.clone(),
        _ => String::new(),
    };
    view.tree()
        .of_kind("ui.Suggestion")
        .into_iter()
        .map(|row| (text(row, "value"), text(row, "label"), text(row, "detail")))
        .collect()
}

/// What the person's To field offers once it holds `typed`.
fn offered_when_typing(typed: &str) -> Vec<(String, String, String)> {
    let (ada, draft) = mailbox();
    offered(&ada, &draft, typed)
}

/// What `draft`'s To field offers once it holds `typed`, on the replica `ada` runs.
fn offered(ada: &Replica, draft: &ResourceRef, typed: &str) -> Vec<(String, String, String)> {
    let to = {
        let package = ada.host.package(MAIL).expect("the mail package");
        field_id(&package.meta, "Draft", "to")
    };
    ada.edit(draft, &to, &Value::text(typed));
    let view = ada
        .host
        .open_resource(draft, None)
        .expect("the draft opens");
    offers(&view)
}

#[test]
fn typing_a_name_in_the_to_field_offers_the_contact_with_their_address() {
    assert_eq!(
        offered_when_typing("an"),
        [(
            "Anna Kowalski <anna@northwind.example>, ".to_owned(),
            "Anna Kowalski".to_owned(),
            "anna@northwind.example".to_owned(),
        )],
        "the contact whose name the words start, written as a header names them"
    );
}

#[test]
fn typing_part_of_an_address_offers_that_contact() {
    assert_eq!(
        offered_when_typing("bo@"),
        [(
            "Bo Diaz <bo@boats.example>, ".to_owned(),
            "Bo Diaz".to_owned(),
            "bo@boats.example".to_owned(),
        )]
    );
}

#[test]
fn a_chosen_contact_stands_after_the_recipients_already_typed() {
    assert_eq!(
        offered_when_typing("bo@boats.example, an"),
        [(
            "bo@boats.example, Anna Kowalski <anna@northwind.example>, ".to_owned(),
            "Anna Kowalski".to_owned(),
            "anna@northwind.example".to_owned(),
        )],
        "taking the row keeps the recipient before it, and ends this one so the next may follow"
    );
}

#[test]
fn a_contact_already_written_is_not_offered_again() {
    assert!(
        offered_when_typing("Anna Kowalski <anna@northwind.example>, ").is_empty(),
        "nothing is typed after the last recipient, and nobody is offered to themselves"
    );
    assert!(
        offered_when_typing("anna@northwind.example, an").is_empty(),
        "the contact already written is not offered again"
    );
}

#[test]
fn a_sent_to_person_is_offered_above_one_who_wrote_and_the_books_name_names_them() {
    let (ada, draft) = mailbox();
    let mail = ada.host.root_of(MAIL).expect("the mail root");
    // One message Ada wrote to a person, and one that person's colleague wrote to her: the
    // mailbox's own records name both, and the book knows the one she wrote to.
    for (from, folder, to) in [
        (
            "Ada Rivera <ada@writing.example>",
            "sent",
            "Robin Sent <robin@one.example>",
        ),
        (
            "Robin Received <robin@two.example>",
            "inbox",
            "ada@writing.example",
        ),
    ] {
        call(
            &ada,
            MAIL,
            "deliver_message",
            vec![
                Value::resource(&mail),
                Value::text(from),
                Value::text("A plan"),
                Value::text("Let us meet."),
                Value::text(folder),
                Value::Bool(true),
                Value::text(to),
            ],
        );
    }
    let book = ada.host.root_of(CONTACTS).expect("the contacts' book");
    let robin = target(&call(
        &ada,
        CONTACTS,
        "new_contact",
        vec![Value::resource(&book), Value::text("Robin Book")],
    ));
    call(
        &ada,
        CONTACTS,
        "add_email",
        vec![
            Value::resource(&robin),
            Value::text("robin@one.example"),
            Value::text("work"),
        ],
    );
    assert_eq!(
        offered(&ada, &draft, "rob"),
        [
            (
                "Robin Book <robin@one.example>, ".to_owned(),
                "Robin Book".to_owned(),
                "robin@one.example".to_owned(),
            ),
            (
                "Robin Received <robin@two.example>, ".to_owned(),
                "Robin Received".to_owned(),
                "robin@two.example".to_owned(),
            ),
        ],
        "the person the mail was written to stands above the one who wrote, under the name the \
         book keeps for them rather than the one the header wrote"
    );
}

/// What a field of a record says, as text.
fn said(r: &Replica, at: &ResourceRef, field: &str) -> String {
    match r.host.runtime().read(at, field).unwrap() {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

/// A `mailto:` link opens a message to whom it names, for the person to read and send; a link
/// that names no address opens nothing.
#[test]
fn a_mailto_link_opens_a_draft_to_whom_it_names() {
    let (ada, _) = mailbox();
    let mail = ada.root(MAIL);
    let opened = call(
        &ada,
        MAIL,
        "open_link",
        vec![
            Value::resource(&mail),
            Value::text("mailto:anna@northwind.example?subject=Hello"),
        ],
    );
    let Value::Opt(Some(draft)) = opened else {
        panic!("the link opens a draft: {opened:?}")
    };
    let draft = target(&draft);
    assert_eq!(said(&ada, &draft, "to"), "anna@northwind.example");
    let opened = call(
        &ada,
        MAIL,
        "open_link",
        vec![
            Value::resource(&mail),
            Value::text("https://example.org/no-address"),
        ],
    );
    assert!(matches!(opened, Value::Opt(None)), "{opened:?}");
}
