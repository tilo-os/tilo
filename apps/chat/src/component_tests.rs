use crate::{ChatApp, doc::ChatDoc, identity::LocalIdentity};
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ui::Widget;
#[test]
fn component_projection_never_seeds_or_marks_read() {
    let identity = LocalIdentity {
        id: "author".into(),
        name: "Author".into(),
        initials: "A".into(),
        color_idx: 1,
    };
    let mut doc = AutoCommit::new();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let before = (doc.base_heads(), doc.pending_ops());
    let mut app =
        ChatApp::from_document(&doc, identity, crate::app::ChatRuntime::Unavailable).unwrap();
    for _ in 0..3 {
        app.sync(&mut doc, &ROOT);
    }
    assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    assert!(!app.resources(&doc).is_empty());
}
#[test]
fn component_projection_refuses_unseeded_record() {
    assert!(
        ChatApp::from_document(
            &AutoCommit::new(),
            LocalIdentity::demo(),
            crate::app::ChatRuntime::Unavailable
        )
        .is_err()
    );
}

#[test]
fn component_local_view_restores_presentation_without_reverting_document() {
    use ui::widget::AppWidget;
    let mut doc = AutoCommit::new();
    let identity = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let mut app =
        ChatApp::from_document(&doc, identity.clone(), crate::app::ChatRuntime::Unavailable)
            .unwrap();
    app.scroll = 125.;
    app.inbox_open = true;
    app.left_flap.snap(false);
    let saved = app
        .capture_application_view()
        .unwrap()
        .expect("Chat local view");
    doc.put(ROOT, "remote-change", "newer").unwrap();
    doc.commit();
    let before = doc.base_heads();
    let mut restored =
        ChatApp::from_document(&doc, identity, crate::app::ChatRuntime::Unavailable).unwrap();
    restored.restore_application_view(&saved, &doc).unwrap();
    assert_eq!(restored.scroll, 125.);
    assert!(restored.inbox_open);
    assert_eq!(doc.base_heads(), before);
}
#[test]
fn canonical_idle_projection_does_not_request_perpetual_frames() {
    let mut doc = AutoCommit::new();
    let me = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &me.id, &me.name, &me.initials);
    doc.commit();
    let mut app = ChatApp::from_document(&doc, me, crate::app::ChatRuntime::Unavailable).unwrap();
    app.mark_drawn();
    assert!(!app.needs_redraw());
}
#[test]
fn canonical_composer_exports_ime_purpose_through_app_root() {
    let mut doc = AutoCommit::new();
    let me = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &me.id, &me.name, &me.initials);
    doc.commit();
    let app = ChatApp::from_document(&doc, me, crate::app::ChatRuntime::Unavailable).unwrap();
    assert!(app.is_focusable());
    assert!(app.ime_purpose().is_some());
}

#[test]
fn two_real_components_open_the_existing_recordless_chat_without_mutation() {
    use std::{cell::RefCell, rc::Rc};
    use ui::{
        application::{HostContext, LaunchTarget, RestoreInput},
        portable_storage::{PortableStorage, StorageKey},
        record::{RecordCheckout, RecordOwners},
    };
    let cache = PortableStorage::new("component-profile", "component-device").unwrap();
    let definition = crate::definition();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
    let mut doc = AutoCommit::new();
    let identity = LocalIdentity::demo();
    // Existing app-state document envelope; opening it must not migrate or restamp.
    doc.put(ROOT, "qos:schema_version", 1u64).unwrap();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let heads = doc.base_heads();
    let actor = doc.get_actor().clone();
    cache
        .hydrate(
            StorageKey::Primary {
                package: definition.app_id.into(),
                workspace: room.into(),
            },
            doc.save(),
            Some(actor.to_bytes().to_vec()),
        )
        .unwrap();
    let records = RecordOwners::portable(cache.clone())
        .open(definition.app_id, room)
        .unwrap();
    let views = Rc::new(ui::application_view::InstanceViewStore::portable(cache, room).unwrap());
    let permissions = Rc::new(RefCell::new(
        ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
            vec![],
        ),
    ));
    let mut components = Vec::new();
    for instance in [1, 2] {
        let target = LaunchTarget::Record {
            checkout: checkout.clone(),
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            records.clone(),
            views.clone(),
            "component-device".into(),
            instance,
            permissions.clone(),
            ui::permissions::UserId("author".into()),
            qos_actions::ActionContext::default(),
            &target,
        )
        .unwrap();
        components.push(
            definition
                .instantiate(context, target, RestoreInput::None)
                .unwrap(),
        );
    }
    assert_eq!(records.route(&checkout).unwrap().record, None);
    assert_eq!(
        records
            .read(&checkout.id, |d| (d.base_heads(), d.get_actor().clone()))
            .unwrap(),
        (heads, actor)
    );
    records
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "peer-message", "shared").unwrap();
        })
        .unwrap();
    assert_eq!(components[0].record(), components[1].record());
    for component in &mut components {
        component.close();
    }
}
