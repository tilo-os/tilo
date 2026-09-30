use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ui::Widget;

#[test]
fn existing_projection_and_repeated_sync_do_not_author_changes() {
    let mut doc = AutoCommit::new();
    word_processor::initialize_empty_document(&mut doc).unwrap();
    doc.commit();
    let before = (doc.base_heads(), doc.pending_ops());
    let mut widget = word_processor::WordProcessorWidget::from_document(&doc).unwrap();
    for _ in 0..3 {
        widget.sync(&mut doc, &ROOT);
    }
    assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    assert!(!ui::resources::resources(&widget, &doc).is_empty());
}

#[test]
fn projection_rejects_missing_schema_instead_of_creating_it() {
    let doc = AutoCommit::new();
    assert!(word_processor::WordProcessorWidget::from_document(&doc).is_err());
    assert_eq!(doc.pending_ops(), 0);
}

fn harness() -> (
    ui::headless::FrameHarness,
    ui::record::RecordStore,
    ui::record::RecordCheckout,
) {
    use std::{cell::RefCell, rc::Rc};
    use ui::application::{
        ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };
    let cache = ui::portable_storage::PortableStorage::new("writer-test", "device").unwrap();
    let definition = word_processor::definition();
    let records = ui::record::RecordOwners::portable(cache.clone())
        .open(definition.app_id, "room")
        .unwrap();
    let views = Rc::new(ui::application_view::InstanceViewStore::portable(cache, "room").unwrap());
    let permissions = Rc::new(RefCell::new(
        ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
            vec![],
        ),
    ));
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records: records.clone(),
            views,
            device_id: "device".into(),
            instance_id: 1,
            permissions,
            author: ui::permissions::UserId("author".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &LaunchTarget::New,
    )
    .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let checkout = component.record().clone();
    let font_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/sdk/ui-core/assets/fonts");
    let fonts = ui::font_catalog::FontCatalog::product(&font_root, "en-US")
        .load_bytes(|name| std::fs::read(font_root.join(name)).ok())
        .unwrap();
    let frame = ui::headless::FrameHarness::from_component(
        component,
        (900, 640),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::with_font_system(fonts),
        },
    )
    .unwrap();
    (frame, records, checkout)
}
#[test]
fn canonical_host_input_and_ime_commit_mutate_the_actual_writer_record() {
    let (mut frame, records, checkout) = harness();
    frame
        .perform_named("word_processor", &ui::a11y::Action::Focus)
        .unwrap();
    frame.type_text("Browser Writer");
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        " 東京".into(),
    )));
    let text = records
        .read(&checkout.id, |doc| {
            serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
        })
        .unwrap();
    assert!(text.contains("Browser Writer 東京"), "{text}");
    assert!(frame.frame_output().ime.is_some());
    if let Ok(out) = std::env::var("TILO_BROWSER_PARITY_OUT") {
        frame.render_png(std::path::Path::new(&out));
    }
    frame.close_components();
}

#[test]
fn ime_edit_is_undoable_through_shared_command_shortcut() {
    use winit::{
        event::{Ime, Modifiers, WindowEvent},
        keyboard::ModifiersState,
    };
    let (mut frame, records, checkout) = harness();
    frame
        .perform_named("word_processor", &ui::a11y::Action::Focus)
        .unwrap();
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::MacOs,
    ));
    frame.send_host_event(&WindowEvent::Ime(Ime::Commit("IME undo probe".into())));
    assert!(frame.undo_status().can_undo);
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
        ModifiersState::SUPER,
    )));
    frame.key("z");
    let text = records
        .read(&checkout.id, |doc| {
            serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
        })
        .unwrap();
    assert!(!text.contains("IME undo probe"), "{text}");
}
