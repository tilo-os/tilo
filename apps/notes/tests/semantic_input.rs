use crate::component_support::*;
use notes::doc::NotesDoc;
use ui::{
    a11y::{AccessNode, Action},
    headless::{Driver, InspectionNode},
};

fn routes(node: &InspectionNode, name: &str, found: &mut Vec<(String, Option<String>)>) {
    fn visit(
        access: &AccessNode,
        owner: &str,
        name: &str,
        found: &mut Vec<(String, Option<String>)>,
    ) {
        if access.name.as_deref() == Some(name) {
            found.push((owner.into(), access.action_target.clone()));
        }
        for child in &access.children {
            visit(child, owner, name, found);
        }
    }
    if let Some(access) = &node.semantics {
        visit(access, &node.path, name, found);
    }
    for child in &node.children {
        routes(child, name, found);
    }
}
fn named(driver: &Driver, name: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    routes(&driver.inspection().root, name, &mut found);
    found
}
fn perform(driver: &mut Driver, route: &(String, Option<String>), action: &Action) {
    driver
        .perform_semantic_named(&route.0, route.1.as_deref(), action)
        .unwrap();
}

#[test]
fn published_new_note_and_page_tokens_use_the_canonical_component_once() {
    let f = LegacyFixture::rich();
    let component = component(&f, 11, ui::application::LaunchTarget::New);
    let checkout = component.record().clone();
    let mut a = Driver::from_component(component, (1000, 700), runner_inputs()).unwrap();
    let mut neighbor = driver(&f, 12, &imported(&f));
    let buttons = named(&a, "New note");
    assert_eq!(buttons.len(), 2);
    let sidebar = buttons.iter().find(|r| r.1.is_none()).unwrap();
    perform(&mut a, sidebar, &Action::Activate);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
            .unwrap(),
        1
    );
    let before = revision(&f, &checkout);
    let page = named(&a, "Untitled")
        .into_iter()
        .find(|r| r.1.as_deref().is_some_and(|t| t.starts_with("node:")))
        .unwrap();
    perform(&mut a, &page, &Action::Activate);
    assert_eq!(revision(&f, &checkout), before, "selection is local");
    for action in [Action::Activate, Action::Focus] {
        assert!(
            a.perform_semantic_named(&page.0, Some("node:999999999"), &action)
                .is_err()
        );
    }
    assert_eq!(named(&neighbor, "Page title").len(), 1);
    close_driver(&mut a);
    close_driver(&mut neighbor);

    let component = component_support_new(&f, 13);
    let checkout = component.record().clone();
    let mut a = Driver::from_component(component, (1000, 700), runner_inputs()).unwrap();
    let first = named(&a, "New note")
        .into_iter()
        .find(|r| r.1.as_deref() == Some("notes.first_note"))
        .unwrap();
    perform(&mut a, &first, &Action::Activate);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
            .unwrap(),
        1
    );
    close_driver(&mut a);
}
fn component_support_new(f: &LegacyFixture, id: u64) -> ui::application::AppComponent {
    component(f, id, ui::application::LaunchTarget::New)
}

#[test]
fn published_title_edits_the_retained_label_and_commits_through_enter_and_blur() {
    let f = LegacyFixture::rich();
    let checkout = imported(&f);
    let mut a = driver(&f, 11, &checkout);
    let page = f.child;
    invoke(
        &mut a,
        11,
        notes::actions::page_resource_id(page),
        "open",
        None,
    );
    let body = f
        .records
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, page)
        })
        .unwrap();
    let title = named(&a, "Page title");
    assert_eq!(title.len(), 1, "one truthful title surface");
    let title = title[0].clone();
    perform(&mut a, &title, &Action::Focus);
    assert_eq!(
        a.focused_access_node()
            .expect("semantic Focus must focus the real retained title")
            .name
            .as_deref(),
        Some("Page title")
    );
    perform(&mut a, &title, &Action::SetValue("Semantic title".into()));
    perform(
        &mut a,
        &title,
        &Action::SetSelection {
            anchor: 2,
            focus: 8,
        },
    );
    a.relayout();
    let focused = a.focused_access_node().unwrap();
    assert_eq!(focused.value.as_deref(), Some("Semantic title"));
    assert_eq!(focused.text_selection.unwrap().focus, 8);
    a.key("Enter");
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc)
                .into_iter()
                .find(|p| p.id == page)
                .unwrap()
                .title)
            .unwrap(),
        "Semantic title"
    );
    perform(&mut a, &title, &Action::Focus);
    perform(&mut a, &title, &Action::SetValue("Blur title".into()));
    a.perform_named("block_editor", &Action::Focus).unwrap();
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc)
                .into_iter()
                .find(|p| p.id == page)
                .unwrap()
                .title)
            .unwrap(),
        "Blur title"
    );
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::page_block_snapshots(
                doc, page
            ))
            .unwrap(),
        body
    );
    invoke(
        &mut a,
        11,
        notes::actions::page_resource_id(f.root),
        "open",
        None,
    );
    assert!(
        a.perform_semantic_named(
            &title.0,
            title.1.as_deref(),
            &Action::SetValue("wrong page".into())
        )
        .is_err()
    );
    assert!(
        a.perform_semantic_named(&title.0, title.1.as_deref(), &Action::Focus)
            .is_err()
    );
    close_driver(&mut a);
}

#[test]
fn title_route_obeys_remote_refresh_replacement_and_revoked_authority() {
    let f = LegacyFixture::rich();
    let checkout = imported(&f);
    let mut a = driver(&f, 11, &checkout);
    let mut b = driver(&f, 12, &checkout);
    let original = named(&a, "Page title")[0].clone();
    invoke(
        &mut b,
        12,
        notes::actions::page_resource_id(f.root),
        "rename",
        Some("Remote title"),
    );
    a.relayout();
    perform(&mut a, &original, &Action::Focus);
    assert_eq!(
        a.focused_access_node().unwrap().value.as_deref(),
        Some("Remote title")
    );
    // Replacing the selected page changes the model token even though the
    // retained label and its inspection path occupy the same layout position.
    invoke(
        &mut a,
        11,
        notes::actions::page_resource_id(f.child),
        "open",
        None,
    );
    let replacement = named(&a, "Page title")[0].clone();
    assert_ne!(original.1, replacement.1);
    assert!(
        a.perform_semantic_named(
            &original.0,
            original.1.as_deref(),
            &Action::SetValue("stale".into())
        )
        .is_err()
    );
    invoke_confirmed(
        &mut b,
        12,
        notes::actions::page_resource_id(f.child),
        "delete",
    );
    a.relayout();
    assert!(
        a.perform_semantic_named(&replacement.0, replacement.1.as_deref(), &Action::Focus)
            .is_err()
    );
    let replacement = named(&a, "Page title")[0].clone();
    *f.authority.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), &f.author.0),
        vec![],
    )
    .at_unix_time(100);
    let before = revision(&f, &checkout);
    perform(&mut a, &replacement, &Action::Focus);
    let canonical = a.focused_access_node().unwrap().value;
    // Local inline staging remains available, as for ordinary keyboard input;
    // the actual canonical rename is authorized when the user commits it.
    perform(&mut a, &replacement, &Action::SetValue("denied".into()));
    a.key("Enter");
    assert_eq!(revision(&f, &checkout), before);
    perform(&mut a, &replacement, &Action::Focus);
    assert_eq!(a.focused_access_node().unwrap().value, canonical);
    let pages = f
        .records
        .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
        .unwrap();
    let create = named(&a, "New note")[0].clone();
    perform(&mut a, &create, &Action::Activate);
    let rect = semantic_rect(&a, "new-note");
    a.click_at(rect.center().x, rect.center().y);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
            .unwrap(),
        pages
    );
    assert_eq!(revision(&f, &checkout), before);
    close_driver(&mut a);
    close_driver(&mut b);
}

#[test]
fn revoked_authority_rejects_empty_notes_semantic_and_pointer_creation() {
    let f = LegacyFixture::rich();
    let component = component(&f, 14, ui::application::LaunchTarget::New);
    let checkout = component.record().clone();
    let mut a = Driver::from_component(component, (1000, 700), runner_inputs()).unwrap();
    *f.authority.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), &f.author.0),
        vec![],
    )
    .at_unix_time(100);
    for route in named(&a, "New note") {
        perform(&mut a, &route, &Action::Activate);
    }
    let rect = semantic_rect(&a, "new-note");
    a.click_at(rect.center().x, rect.center().y);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::list_pages(doc).len())
            .unwrap(),
        0
    );
    assert_eq!(revision(&f, &checkout), 0);
    close_driver(&mut a);
}
