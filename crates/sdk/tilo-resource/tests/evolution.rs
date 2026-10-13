//! Storage versions, migrations, unreadable documents and the public
//! surface.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use automerge::ReadDoc;
use automerge::transaction::Transactable;
use tilo_resource::evolution::{self, VERSION_KEY};
use tilo_resource::{ActionKind, Model, ModelError, Outbox, Reader, Replica, Resource, resource};

/// The first version: a contact with one `name`.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ContactV1 {
    pub name: String,
}

#[resource(kind = "test.contact", label = "Contact")]
impl ContactV1 {
    /// Rename the contact.
    #[action]
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }
}

/// The second: the name split in two.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Contact {
    /// Given name.
    #[model(label)]
    pub given: String,
    pub family: String,
    #[model(readonly, unit)]
    pub notes: String,
    pub outbox: Outbox<String>,
}

#[resource(kind = "test.contact", label = "Contact", version = 2, migrate = Contact::upgrade)]
impl Contact {
    /// Rename the contact.
    #[action]
    pub fn rename(&mut self, given: String, family: String) {
        self.given = given;
        self.family = family;
    }

    /// Invite the contact.
    #[action(destructive)]
    pub fn invite(&mut self, cx: &mut tilo_resource::ActionCx, outbox: &mut Outbox<String>) {
        outbox.push(cx.id(), self.given.clone());
    }

    fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
        if from != 1 {
            return Err(ModelError::invalid(format!("no migration from {from}")));
        }
        let name: String = tilo_resource::read_key(doc, &automerge::ROOT, "name")?;
        let (given, family) = name.split_once(' ').unwrap_or((&name, ""));
        tilo_resource::write_key(doc, &automerge::ROOT, "given", &given.to_owned())?;
        tilo_resource::write_key(doc, &automerge::ROOT, "family", &family.to_owned())?;
        doc.delete(automerge::ROOT, "name")?;
        Ok(())
    }
}

fn old_document() -> tilo_resource::Doc {
    let mut old = Replica::<ContactV1>::new();
    old.apply(ContactV1::RENAME.with("Ada Lovelace")).unwrap();
    old.doc_mut().fork()
}

#[test]
fn an_old_document_migrates_once_and_reads_as_the_new_model() {
    let mut doc = old_document();
    assert_eq!(evolution::stored_version(&doc), 1);
    // Unmigrated, it does not read: it never shows stale state as current.
    let unreadable = evolution::read_checked::<Contact>(&doc).unwrap_err();
    assert_eq!((unreadable.stored, unreadable.expected), (1, 2));
    assert!(Contact::read(&doc).is_err());

    assert!(evolution::migrate::<Contact>(&mut doc).unwrap());
    assert!(!evolution::migrate::<Contact>(&mut doc).unwrap(), "once");
    let contact = evolution::read_checked::<Contact>(&doc).unwrap();
    assert_eq!(
        (contact.given.as_str(), contact.family.as_str()),
        ("Ada", "Lovelace")
    );
    let version = tilo_resource::history::versions(&mut doc);
    let last = version.last().unwrap();
    assert!(
        last.context
            .as_ref()
            .is_some_and(tilo_actions::ActionContext::is_housekeeping),
        "a migration is housekeeping"
    );
    let mut replica = Replica::<Contact>::from_doc(doc);
    replica
        .apply(Contact::RENAME.with(("Grace".to_owned(), "Hopper".to_owned())))
        .unwrap();
    assert_eq!(replica.state().unwrap().given, "Grace");
}

#[test]
fn two_devices_migrating_concurrently_make_the_same_change() {
    let mut left = old_document();
    let mut right = left.fork();
    evolution::migrate::<Contact>(&mut left).unwrap();
    evolution::migrate::<Contact>(&mut right).unwrap();
    assert_eq!(left.get_heads(), right.get_heads());
    tilo_automerge::ApplyIncoming::merge_incoming(&mut left, &mut right).unwrap();
    assert_eq!(left.get_heads().len(), 1);
    assert_eq!(Contact::read(&left).unwrap().given, "Ada");
}

#[test]
fn a_document_from_a_newer_app_is_unreadable() {
    let mut replica = Replica::<Contact>::new();
    assert_eq!(evolution::stored_version(replica.doc()), 2);
    let mut reader = Reader::<Contact>::open(replica.doc_mut()).unwrap();
    replica
        .doc_mut()
        .put(
            automerge::ROOT,
            VERSION_KEY,
            automerge::ScalarValue::Uint(3),
        )
        .unwrap();
    replica.doc_mut().commit();
    assert!(reader.sync(replica.doc()).is_err());
    let unreadable = reader.unreadable().unwrap();
    assert_eq!((unreadable.stored, unreadable.expected), (3, 2));
    assert!(unreadable.reason.contains("newer version"), "{unreadable}");
    assert!(
        replica
            .doc()
            .get(automerge::ROOT, "given")
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_surface_lists_verbs_kinds_and_fields() {
    let surface = Contact::surface();
    assert_eq!(surface.kind, "test.contact");
    assert_eq!(surface.version, 2);
    let invite = surface
        .verbs
        .iter()
        .find(|verb| verb.id == "invite")
        .unwrap();
    assert_eq!(invite.kind, ActionKind::Outside);
    assert!(invite.destructive);
    assert!(invite.params.is_empty());
    let rename = surface
        .verbs
        .iter()
        .find(|verb| verb.id == "rename")
        .unwrap();
    assert_eq!(rename.kind, ActionKind::Undoable);
    assert_eq!(
        rename
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        ["given", "family"]
    );
    assert_eq!(rename.params[0].schema["type"], "string");
    let notes = surface
        .fields
        .iter()
        .find(|field| field.path == "notes")
        .unwrap();
    assert!(notes.readonly && notes.unit);
    let given = surface
        .fields
        .iter()
        .find(|field| field.path == "given")
        .unwrap();
    assert!(given.label);
    assert_eq!(given.title, "Given name");
    assert_eq!(given.ty, "String");
    assert!(
        surface
            .fields
            .iter()
            .any(|field| field.path == "outbox" && field.ty == "Outbox<String>")
    );
    // It is data: it survives its own serialization.
    let json = serde_json::to_string(&surface).unwrap();
    assert_eq!(
        serde_json::from_str::<tilo_resource::Surface>(&json).unwrap(),
        surface
    );
}

#[test]
fn a_replica_acts_through_the_door() {
    let mut contact = Replica::<Contact>::new();
    // A destructive verb asks the person first.
    assert!(matches!(
        contact.apply(Contact::INVITE.invoke()),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    assert_eq!(contact.state().unwrap().outbox.len(), 0);
    contact.apply_confirmed(Contact::INVITE.invoke()).unwrap();
    assert_eq!(contact.state().unwrap().outbox.len(), 1);
    // An action the resource does not publish is unknown.
    let unknown: tilo_resource::ActionRef<Contact, ()> = tilo_resource::ActionRef::new("nope", &[]);
    assert!(matches!(
        contact.apply(unknown.invoke()),
        Err(tilo_actions::DoorError::Unknown(_))
    ));
    // The change carries the history entry the door writes.
    contact
        .apply(Contact::RENAME.with(("Ada".to_owned(), "King".to_owned())))
        .unwrap();
    let versions = contact.versions();
    let context = versions.last().unwrap().context.clone().unwrap();
    let Some(tilo_actions::Described {
        what: tilo_actions::Did::Verb(verb),
        ..
    }) = context.described()
    else {
        panic!("the change names its verb: {context:?}");
    };
    assert_eq!(verb.action.action, "rename");
    assert_eq!(verb.action.kind, "test.contact");
}
