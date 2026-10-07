// Included in the actual palette fixture; final dispatch is the ordinary registry.
fn deferred_palette_resource(calls: Arc<std::sync::atomic::AtomicUsize>) -> Resource {
    use tilo_actions::{ContinuationEvidence, ContinuationKey, ContinuationQuery};
    Resource::new("test.thing:1", "test.thing", "Thing").with_action(
        ActionSpec::deferred(
            "create",
            "Create",
            tilo_actions::ExecutionLocus::ClientLocal,
            NOTE_TYPE,
            move || {
                let value = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                EncodedAction::encode(
                    NOTE_TYPE,
                    &Note {
                        key: "created".into(),
                        value: value.to_string(),
                    },
                )
                .unwrap()
            },
        )
        .with_continuation_identity(|query| {
            matches!(query, ContinuationQuery::Draft).then(|| ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new("palette-create", [b"thing"]),
                handler: ContinuationKey::new("palette-create", [b"v1"]),
            })
        }),
    )
}
#[test]
fn deferred_palette_listing_activation_and_take_intent_never_mint() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let resource = deferred_palette_resource(calls.clone());
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    for _ in 0..3 {
        palette.load_open_resources(std::slice::from_ref(&resource));
        palette.open();
    }
    assert!(palette.activate_address("test.thing:1", "create"));
    let pending = palette.take_operation().expect("current retained intent");
    assert!(pending.encoded().is_none(), "no fake encoded value exists");
    assert_eq!(pending.wire_argument(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn deferred_palette_final_local_dispatch_mints_once_per_new_activation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let resource = deferred_palette_resource(calls.clone());
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.load_open_resources(&[resource]);
    let mut doc = automerge::AutoCommit::new();
    let mut tx = LocalTransport::new();
    for expected in 0..2 {
        palette.open();
        assert!(palette.activate_address("test.thing:1", "create"));
        assert_eq!(calls.load(Ordering::SeqCst), expected);
        assert!(matches!(
            palette
                .take_dispatch(
                    &mut crate::widget::Edits::lend(&mut doc),
                    &registry(),
                    &mut tx
                )
                .unwrap()
                .unwrap(),
            Dispatched::AppliedLocal(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), expected + 1);
        assert_eq!(
            doc.get(ROOT, "created").unwrap().unwrap().0.as_str(),
            Some(expected.to_string().as_str())
        );
    }
}
#[test]
fn deferred_palette_changed_declaration_discards_intent_without_mint() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let mut resource = deferred_palette_resource(calls.clone());
    let mut palette = ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    });
    palette.load_open_resources(std::slice::from_ref(&resource));
    palette.open();
    assert!(palette.activate_address("test.thing:1", "create"));
    resource.actions[0].label = "Changed meaning".into();
    palette.load_open_resources(&[resource]);
    assert!(palette.take_operation().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
