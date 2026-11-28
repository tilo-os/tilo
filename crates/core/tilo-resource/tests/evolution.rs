use automerge::transaction::Transactable;
use tilo_resource::evolution::{self, VERSION_KEY};
use tilo_resource::{ActionKind, Model, ModelError, Outbox, Reader, Replica, Resource, resource};

/// The first version: a contact with one `name`.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ContactV1 {
    pub name: String,
}

#[resource(kind = "test.contact", catalogue = "tests/locales")]
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

#[resource(kind = "test.contact", version = 2, migrate = Contact::upgrade, catalogue = "tests/locales")]
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
        // The field the older version wrote stays: a version only adds fields.
        Ok(())
    }
}

fn old_document() -> tilo_resource::Doc {
    let mut old = Replica::<ContactV1>::new().in_app("so.tilo.model", "Model");
    old.apply(ContactV1::RENAME.with("Ada Lovelace")).unwrap();
    old.doc_mut().fork()
}

tilo_test_env::scenario! {
    fn a_document_migrates_to_the_model_that_reads_it;

    phase an_old_document_migrates_once_and_reads_as_the_new_model;
    {
        let mut doc = old_document();
        assert_eq!(evolution::stored_version(&doc), 1);
        // Unmigrated, it does not read: it never shows stale state as current.
        let unreadable = evolution::read_checked::<Contact>(&doc).unwrap_err();
        assert_eq!((unreadable.stored, unreadable.expected), (1, 2));
        assert!(Contact::read(&doc).is_err());

        assert!(crate::bring_up::<Contact>(&mut doc));
        assert!(!crate::bring_up::<Contact>(&mut doc), "once");
        use tilo_resource::founding::{containers, found_holding, structure, Scope};
        use tilo_resource::surface::surface_of;
        let scope = Scope {
            workspace: Replica::<Contact>::WORKSPACE,
            kind: Contact::KIND,
            id: Replica::<Contact>::ID,
        };
        let now = containers(&surface_of::<Contact>());
        let older = containers(&surface_of::<ContactV1>());
        let before = doc.get_changes(&[]).len();
        let answered = found_holding((scope, "so.tilo.model"), &now, &mut doc).unwrap();
        assert_eq!(answered, (false, structure(scope, &older).unwrap().hash()));
        assert_ne!(answered.1, structure(scope, &now).unwrap().hash());
        assert_eq!(doc.get_changes(&[]).len(), before, "a reopen writes nothing");        let contact = evolution::read_checked::<Contact>(&doc).unwrap();
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
        let mut replica = Replica::<Contact>::from_doc(doc).in_app("so.tilo.model", "Model");
        replica
            .apply(Contact::RENAME.with(("Grace".to_owned(), "Hopper".to_owned())))
            .unwrap();
        assert_eq!(replica.state().unwrap().given, "Grace");
    }

    phase the_document_before_a_migration_stays_readable_in_its_history;
    // A migration is one change on top of the document's history, so the
    // document as it was before it is the snapshot a rollback reads: the
    // previous build finds its own version there, whole.
    {
        let mut doc = old_document();
        let before = doc.get_heads();
        assert!(crate::bring_up::<Contact>(&mut doc));
        let snapshot = doc.fork_at(&before).unwrap();
        assert_eq!(evolution::stored_version(&snapshot), 1);
        assert_eq!(ContactV1::read(&snapshot).unwrap().name, "Ada Lovelace");
        // The build that wrote it reads the field it knows from the migrated
        // document too: a newer version only added to the type.
        assert_eq!(
            evolution::read_checked::<ContactV1>(&doc).unwrap().name,
            "Ada Lovelace"
        );
    }

    phase two_devices_migrating_concurrently_make_the_same_change;
    {
        let mut left = old_document();
        let mut right = left.fork();
        crate::bring_up::<Contact>(&mut left);
        crate::bring_up::<Contact>(&mut right);
        assert_eq!(left.get_heads(), right.get_heads());
        tilo_automerge::ApplyIncoming::merge_incoming(&mut left, &mut right).unwrap();
        assert_eq!(left.get_heads().len(), 1);
        assert_eq!(Contact::read(&left).unwrap().given, "Ada");
    }

    phase a_document_from_a_newer_app_reads_what_this_build_knows;
    {
        let mut replica = Replica::<Contact>::new().in_app("so.tilo.model", "Model");
        assert_eq!(evolution::stored_version(replica.doc()), 2);
        let mut reader = Reader::<Contact>::open(replica.doc_mut()).unwrap();
        // A newer version of a type only adds to it: this build's fields are all still there,
        // and it reads them while the field the newer version added waits for a build that
        // declares it.
        replica
            .doc_mut()
            .put(
                automerge::ROOT,
                VERSION_KEY,
                automerge::ScalarValue::Uint(3),
            )
            .unwrap();
        replica
            .doc_mut()
            .put(automerge::ROOT, "from_the_future", "x")
            .unwrap();
        replica.doc_mut().commit();
        assert!(reader.sync(replica.doc()).is_ok());
        assert!(reader.unreadable().is_none());
        assert_eq!(reader.state().given, "");
    }
}

tilo_test_env::scenario! {
    fn a_record_acts_through_the_surface_it_lists;

    phase the_surface_lists_verbs_kinds_and_fields;
    {
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
        assert_eq!(given.ty, tilo_resource::FieldType::Text);
        assert!(surface.fields.iter().any(|field| field.path == "outbox"
            && field.ty == tilo_resource::FieldType::Outbox(Box::new(tilo_resource::FieldType::Text))));
        // It is data: it survives its own serialization.
        let json = serde_json::to_string(&surface).unwrap();
        assert_eq!(
            serde_json::from_str::<tilo_resource::Surface>(&json).unwrap(),
            surface
        );
    }

    phase a_group_holding_a_change_undo_cannot_take_back_asks_first;
    // A group is undoable only when every member is: one that sends something
    // away makes a deleting group ask, through any API, as that member alone
    // does.
    {
        let mut contact = Replica::<Contact>::new().in_app("so.tilo.model", "Model");
        let group = || {
            tilo_resource::Invoke::group([
                Contact::RENAME.with(("Ada".to_owned(), "King".to_owned())),
                Contact::INVITE.invoke(),
            ])
        };
        assert!(matches!(
            contact.apply(group()),
            Err(tilo_actions::ApiError::ConfirmationRequired)
        ));
        assert_eq!(contact.state().unwrap().outbox.len(), 0);
        contact.apply_confirmed(group()).unwrap();
        assert_eq!(contact.state().unwrap().outbox.len(), 1);
    }

    phase a_replica_acts_through_the_api;
    {
        let mut contact = Replica::<Contact>::new().in_app("so.tilo.model", "Model");
        // A destructive verb asks the person first.
        assert!(matches!(
            contact.apply(Contact::INVITE.invoke()),
            Err(tilo_actions::ApiError::ConfirmationRequired)
        ));
        assert_eq!(contact.state().unwrap().outbox.len(), 0);
        contact.apply_confirmed(Contact::INVITE.invoke()).unwrap();
        assert_eq!(contact.state().unwrap().outbox.len(), 1);
        // An action the resource does not publish is unknown.
        let unknown: tilo_resource::ActionRef<Contact, ()> =
            tilo_resource::ActionRef::new("nope", &[], tilo_resource::ResourceKind::of::<Contact>());
        assert!(matches!(
            contact.apply(unknown.invoke()),
            Err(tilo_actions::ApiError::Unknown(_))
        ));
        // The change carries the history entry the API writes.
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

    phase a_record_copied_into_another_store_reads_the_same_and_keeps_its_identity;
    // A record moving to another store is copied into a record of the same
    // kind made there: it then reads as the one it came from, its lists and
    // texts whole, and stays the record it was made as.
    {
        let mut from = Replica::<Contact>::new().in_app("so.tilo.model", "Model");
        from.apply(Contact::RENAME.with(("Ada".to_owned(), "Lovelace".to_owned())))
            .unwrap();
        from.apply_confirmed(Contact::INVITE.invoke()).unwrap();
        let mut to = Replica::<Contact>::new().in_app("so.tilo.model", "Model");
        to.doc_mut()
            .put(automerge::ROOT, "tilo:home", "the other store")
            .unwrap();
        evolution::copy_into(from.doc(), to.doc_mut()).unwrap();
        let copied = Contact::read(to.doc()).unwrap();
        let original = from.state().unwrap();
        assert_eq!((copied.given.as_str(), copied.family.as_str()), ("Ada", "Lovelace"));
        assert_eq!(copied.outbox.len(), original.outbox.len());
        assert_eq!(copied.notes, original.notes);
        let home: String = tilo_resource::read_key(to.doc(), &automerge::ROOT, "tilo:home").unwrap();
        assert_eq!(home, "the other store", "the record keeps its own identity");
        // It goes on as a record of its own: actions apply to the copy.
        let mut to = Replica::<Contact>::from_doc(to.doc_mut().fork()).in_app("so.tilo.model", "Model");
        to.apply(Contact::RENAME.with(("Grace".to_owned(), "Hopper".to_owned())))
            .unwrap();
        assert_eq!(to.state().unwrap().given, "Grace");
        assert_eq!(from.state().unwrap().given, "Ada");
    }
}

/// The framework's own content verb is part of the surface: a record whose fields a person types
/// into declares `type`. A storage migration is the store's own verb, which no surface declares,
/// whether or not the type registers a migration.
#[test]
fn the_surface_declares_the_framework_content_verbs_the_kind_takes() {
    let surface = Contact::surface();
    assert!(surface.verb("type").is_some(), "its names are typed into");
    assert!(
        surface.verb("migrate").is_none(),
        "the store's migration verb is no surface's"
    );
    let first = ContactV1::surface();
    assert!(first.verb("type").is_some());
    assert!(first.verb("migrate").is_none());
}
