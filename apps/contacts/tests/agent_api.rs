//! What an agent does with the address book through published actions
//! alone: create contacts with their fields, read them back, edit them by
//! id, and take them away as a vCard file kept as content.

use contacts::ContactsShell;
use contacts::actions::{ContactsEffect, contact_resource_id};
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_resource::Effect as _;

fn new_contact(app: &mut TestApp<ContactsShell>, arguments: serde_json::Value) -> String {
    let answer = app
        .call("new-contact", arguments)
        .unwrap()
        .expect("new-contact answers what it made");
    let id = answer["id"]
        .as_str()
        .expect("the new contact's id")
        .to_owned();
    assert_eq!(answer["resource"], contact_resource_id(id.parse().unwrap()));
    id
}

fn list(app: &mut TestApp<ContactsShell>, arguments: serde_json::Value) -> serde_json::Value {
    app.call("list-contacts", arguments)
        .unwrap()
        .expect("list-contacts answers the contacts")
}

fn read(app: &mut TestApp<ContactsShell>, id: &str) -> serde_json::Value {
    app.call("read-contact", json!({ "id": id }))
        .unwrap()
        .expect("read-contact answers the contact")
}

/// Two contacts created with their role, email and notes.
fn team() -> (TestApp<ContactsShell>, String, String) {
    let mut app = TestApp::<ContactsShell>::new().unwrap();
    let ada = new_contact(
        &mut app,
        json!({
            "value": "Ada Lovelace",
            "org": "Analytical Engines",
            "title": "Chief Engineer",
            "email": "ada@engines.test",
            "phone": "+44 20 7946 0000",
            "notes": "Dietary: vegetarian",
            "group": "Speakers",
        }),
    );
    let alan = new_contact(&mut app, json!({ "value": "Alan Turing" }));
    (app, ada, alan)
}

/// The reads are views a planner may run while it plans: they change
/// nothing, answer JSON, need read permission and ask no confirmation.
#[test]
fn the_reads_are_views_that_answer_json() {
    let specs = ContactsEffect::specs();
    for id in ["list-contacts", "read-contact"] {
        let spec = specs.iter().find(|spec| spec.id == id).expect(id);
        assert_eq!(spec.kind, tilo_actions::OperationKind::View, "{id}");
        assert!(
            matches!(spec.contract.result, tilo_actions::ResultSpec::Json { .. }),
            "{id}"
        );
        assert_eq!(
            spec.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        assert_eq!(
            spec.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        );
    }
    let export = specs
        .iter()
        .find(|spec| spec.id == "export-vcard-content")
        .expect("export-vcard-content");
    assert_eq!(
        export.contract.result,
        tilo_actions::ResultSpec::Resource {
            resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
        }
    );
    assert!(
        !export
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
        "keeping contacts as content needs no device file chooser"
    );
}

#[test]
fn created_contacts_list_and_read_back_with_their_fields() {
    let (mut app, ada, alan) = team();

    let all = list(&mut app, json!({}));
    assert_eq!(all["total"], 2);
    let names: Vec<_> = all["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|contact| contact["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["Ada Lovelace", "Alan Turing"]);
    let first = &all["contacts"][0];
    assert_eq!(first["id"], ada.as_str());
    assert_eq!(first["org"], "Analytical Engines");
    assert_eq!(first["title"], "Chief Engineer");
    assert_eq!(first["emails"], json!(["ada@engines.test"]));
    assert_eq!(first["phones"], json!(["+44 20 7946 0000"]));
    assert_eq!(first["notes"], "Dietary: vegetarian");
    assert_eq!(first["groups"], json!(["Speakers"]));

    let found = list(&mut app, json!({ "query": "vegetarian" }));
    assert_eq!(found["total"], 1);
    assert_eq!(found["contacts"][0]["id"], ada.as_str());
    let grouped = list(&mut app, json!({ "group": "speakers" }));
    assert_eq!(grouped["total"], 1, "groups match case-insensitively");
    assert_eq!(list(&mut app, json!({ "query": "nobody" }))["total"], 0);

    let full = read(&mut app, &ada);
    assert_eq!(full["first"], "Ada");
    assert_eq!(full["last"], "Lovelace");
    assert_eq!(full["emails"][0]["value"], "ada@engines.test");
    assert!(full["emails"][0]["id"].is_string(), "rows carry entry ids");
    assert_eq!(read(&mut app, &alan)["title"], "");
    assert!(
        app.call(
            "read-contact",
            json!({ "id": uuid::Uuid::nil().to_string() })
        )
        .is_err(),
        "an unknown id is refused, not answered empty"
    );
    assert_eq!(app.state().contacts.len(), 2, "reading changed nothing");
}

#[test]
fn fields_are_set_by_id_and_unknown_keys_are_refused() {
    let (mut app, ada, alan) = team();

    app.call(
        "update-contact",
        json!({ "id": alan, "title": "Cryptanalyst", "email": "alan@bletchley.test",
                "notes": "Dietary: none" }),
    )
    .unwrap();
    let alan_read = read(&mut app, &alan);
    assert_eq!(alan_read["title"], "Cryptanalyst");
    assert_eq!(alan_read["emails"][0]["value"], "alan@bletchley.test");
    assert_eq!(alan_read["notes"], "Dietary: none");

    let resource = contact_resource_id(ada.parse().unwrap());
    app.call_on(
        &resource,
        "update-fields",
        json!({ "patch": { "job_title": "Countess", "organisation": "Royal Society",
                           "email": "countess@society.test" } }),
    )
    .unwrap();
    let ada_read = read(&mut app, &ada);
    assert_eq!(ada_read["title"], "Countess");
    assert_eq!(ada_read["org"], "Royal Society");
    let emails = ada_read["emails"].as_array().unwrap();
    assert_eq!(emails.len(), 1, "email edits the primary address");
    assert_eq!(emails[0]["value"], "countess@society.test");

    let refused = app
        .call_on(
            &resource,
            "update-fields",
            json!({ "patch": { "work_email": "x@y.test" } }),
        )
        .expect_err("an unknown field is refused");
    let message = refused.to_string();
    assert!(message.contains("work_email"), "{message}");
    assert!(
        message.contains("title"),
        "the valid keys are named: {message}"
    );
    assert_eq!(read(&mut app, &ada)["title"], "Countess", "nothing changed");
}

#[test]
fn contacts_export_as_a_vcard_kept_as_content_and_parse_back() {
    let (mut app, _, _) = team();

    let output = app
        .effect(&ContactsEffect::ExportVcardContent {
            query: None,
            group: None,
        })
        .unwrap();
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = address
        .strip_prefix("tilo.content:")
        .expect("a content address");
    let cards = String::from_utf8(app.content().read(hash).expect("the bytes are kept")).unwrap();
    let parsed = contacts::vcard::parse_vcards(&cards);
    assert_eq!(parsed.len(), 2);
    let ada = parsed
        .iter()
        .find(|contact| contact.first == "Ada")
        .expect("Ada is exported");
    assert_eq!(ada.title, "Chief Engineer");
    assert_eq!(ada.org, "Analytical Engines");
    assert_eq!(ada.notes, "Dietary: vegetarian");
    assert_eq!(ada.emails[0].value, "ada@engines.test");

    let output = app
        .effect(&ContactsEffect::ExportVcardContent {
            query: None,
            group: Some("Speakers".into()),
        })
        .unwrap();
    let hash = output.resources[0].strip_prefix("tilo.content:").unwrap();
    let cards = String::from_utf8(app.content().read(hash).unwrap()).unwrap();
    let parsed = contacts::vcard::parse_vcards(&cards);
    assert_eq!(parsed.len(), 1, "a group exports only its members");
    assert_eq!(parsed[0].last, "Lovelace");
}
