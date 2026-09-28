use std::{cell::RefCell, rc::Rc};

use ui::application::{
    ApplicationDefinition, ComponentRunnerInputs, HostContext, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::bound_dispatch::BoundActionAddress;
use ui::headless::FrameHarness;
use ui::permissions::{Actor, PermissionContext, Role, UserId};
use ui::portable_storage::PortableStorage;
use ui::record::{RecordCheckout, RecordStore};

use crate::doc::BooksDoc;
use crate::model::AnnoKind;

const DEVICE: &str = "books-device";
const WORKSPACE: &str = "books-room";

struct Fixture {
    records: RecordStore,
    views: Rc<InstanceViewStore>,
    authority: Rc<RefCell<PermissionContext>>,
}

impl Fixture {
    fn new(profile: &str) -> Self {
        let cache = PortableStorage::new(profile, DEVICE).unwrap();
        Self {
            records: ui::record::RecordOwners::portable(cache.clone())
                .open(crate::definition().app_id, WORKSPACE)
                .unwrap(),
            views: Rc::new(InstanceViewStore::portable(cache, WORKSPACE).unwrap()),
            authority: Rc::new(RefCell::new(PermissionContext::from_capabilities(
                Actor::person(Some(Role::Owner), "reader"),
                vec![],
            ))),
        }
    }

    fn driver(
        &self,
        definition: &'static ApplicationDefinition,
        instance: u64,
        target: LaunchTarget,
    ) -> (FrameHarness, RecordCheckout) {
        let Ok(context) = HostContext::prepare(
            definition,
            self.records.clone(),
            self.views.clone(),
            DEVICE.into(),
            instance,
            self.authority.clone(),
            UserId("reader".into()),
            tilo_actions::ActionContext::default(),
            &target,
        ) else {
            panic!("host context");
        };
        let Ok(component) = definition.instantiate(context, target, RestoreInput::None) else {
            panic!("Books component");
        };
        let checkout = component.record().clone();
        let Ok(mut driver) = FrameHarness::from_component(
            component,
            (1100, 760),
            ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::new(),
            },
        ) else {
            panic!("frame harness");
        };
        settle(&mut driver);
        (driver, checkout)
    }
}

fn bundled_book() -> String {
    format!("ebook.book:{}", crate::content::bundled_book_id())
}

/// Frames until the book's search index (built off the UI thread natively)
/// resolves the bundled book's resource, if the library holds it.
fn settle(driver: &mut FrameHarness) {
    for _ in 0..100 {
        driver.relayout();
        if driver
            .bound_resources(Some(&bundled_book()))
            .ok()
            .is_some_and(|resources| !resources.is_empty())
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn invoke(
    driver: &mut FrameHarness,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) -> Result<(), tilo_actions::DoorError> {
    let address = BoundActionAddress {
        instance_id: driver.component_instance_id().expect("component"),
        address: tilo_actions::ActionAddress::new(resource, action),
    };
    let serde_json::Value::Object(arguments) = arguments else {
        return Err(tilo_actions::DoorError::InvalidArguments(
            "arguments are an object".into(),
        ));
    };
    driver.invoke_bound_action(&address, &arguments)?;
    driver.relayout();
    Ok(())
}

fn child_labels(driver: &FrameHarness, book: &str) -> Vec<(String, String)> {
    let resource = driver.bound_resources(Some(book)).unwrap();
    resource[0]
        .children
        .iter()
        .map(|r| (r.kind.clone(), r.label.clone()))
        .collect()
}

#[test]
fn a_native_new_library_starts_empty_on_the_shelf() {
    let fixture = Fixture::new("books-native-new");
    let (driver, checkout) = fixture.driver(crate::definition(), 1, LaunchTarget::New);
    let shelf = fixture
        .records
        .read(&checkout.id, BooksDoc::read_library_entries)
        .unwrap();
    assert!(shelf.is_empty(), "apps start empty: {shelf:?}");
    let book = driver.bound_resources(Some(&bundled_book())).unwrap();
    assert!(book.is_empty(), "the native shelf holds no bundled book");
}

#[test]
fn the_browser_library_opens_the_bundled_book_and_annotates_it_through_actions() {
    let fixture = Fixture::new("books-browser-flow");
    let definition = super::bundled_library_definition();
    let (mut first, checkout) = fixture.driver(definition, 1, LaunchTarget::New);
    let book = bundled_book();
    let target = LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    };
    let (mut second, _) = fixture.driver(definition, 2, target);

    let resource = first.bound_resources(Some(&book)).unwrap();
    let actions: Vec<_> = resource[0].actions.iter().map(|a| a.id.as_str()).collect();
    assert!(actions.contains(&"select-passage"), "{actions:?}");
    assert!(
        !actions.contains(&"highlight-selection"),
        "nothing is selected yet"
    );

    // Paragraphs 1–2 of chapter 1 (block 0 is its heading).
    invoke(
        &mut first,
        &book,
        "select-passage",
        serde_json::json!({"start_block": 1, "end_block": 2, "end_char": 40}),
    )
    .expect("select");
    let resource = first.bound_resources(Some(&book)).unwrap();
    let actions: Vec<_> = resource[0].actions.iter().map(|a| a.id.as_str()).collect();
    assert!(actions.contains(&"highlight-selection"), "{actions:?}");
    assert!(actions.contains(&"note-selection"), "{actions:?}");

    invoke(
        &mut first,
        &book,
        "highlight-selection",
        serde_json::json!({}),
    )
    .expect("highlight");
    invoke(
        &mut first,
        &book,
        "note-selection",
        serde_json::json!({"value": "The keeper's debt"}),
    )
    .expect("note");

    let annotations = fixture
        .records
        .read(&checkout.id, |doc| {
            BooksDoc::annotations_for(doc, &crate::content::bundled_book_id())
        })
        .unwrap();
    let highlights: Vec<_> = annotations
        .iter()
        .filter(|a| a.kind == AnnoKind::Highlight)
        .collect();
    assert_eq!(
        highlights.iter().map(|a| a.block).collect::<Vec<_>>(),
        [1, 2],
        "one highlight row per selected paragraph"
    );
    assert_eq!(highlights[1].end, 40);
    let note = annotations
        .iter()
        .find(|a| a.kind == AnnoKind::Note)
        .expect("note");
    assert_eq!(note.note, "The keeper's debt");
    assert_eq!((note.chapter, note.block, note.start), (0, 1, 0));
    assert!(
        note.excerpt.starts_with("The lantern at Cape Marrow"),
        "{:?}",
        note.excerpt
    );

    // The annotations read back as the open book's resources.
    let children = child_labels(&first, &book);
    assert!(
        children.contains(&("ebook.note".into(), "The keeper's debt".into())),
        "{children:?}"
    );
    assert!(
        children.iter().any(|(kind, _)| kind == "ebook.highlight"),
        "{children:?}"
    );

    // The second mount of the record shares the document: once its frame
    // passes run again (the browser runtime re-syncs it when the first
    // mount's record changes), it reads the first mount's annotations.
    second.relayout();
    assert!(
        child_labels(&second, &book)
            .iter()
            .any(|(_, label)| label == "The keeper's debt"),
        "the second mount reads the first mount's note"
    );
}

#[test]
fn a_passage_outside_the_chapter_is_refused_without_a_selection() {
    let fixture = Fixture::new("books-browser-refusal");
    let (mut driver, _) = fixture.driver(super::bundled_library_definition(), 1, LaunchTarget::New);
    let book = bundled_book();
    assert!(
        invoke(
            &mut driver,
            &book,
            "select-passage",
            serde_json::json!({"start_block": 900}),
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut driver,
            &book,
            "select-passage",
            serde_json::json!({"start_block": -1}),
        )
        .is_err(),
        "a negative block is refused by the parameter schema"
    );
    let resource = driver.bound_resources(Some(&book)).unwrap();
    assert!(
        resource[0]
            .actions
            .iter()
            .all(|action| action.id != "highlight-selection")
    );
}
