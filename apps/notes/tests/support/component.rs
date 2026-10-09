//! Explicit temporary canonical records and controlled runner fonts.
#[path = "canonical.rs"]
pub mod canonical;
pub use canonical::NotebookFixture;
use std::rc::Rc;
use ui::application::{AppComponent, HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::application_view::InstanceViewStore;
use ui::record::RecordCheckout;
use ui::widget::Edits;

pub fn existing(checkout: &RecordCheckout) -> LaunchTarget {
    LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    }
}
pub fn component(f: &NotebookFixture, instance: u64, target: LaunchTarget) -> AppComponent {
    restored_component(f, instance, target, RestoreInput::None)
}
pub fn restored_component(
    f: &NotebookFixture,
    instance: u64,
    target: LaunchTarget,
    restore: RestoreInput,
) -> AppComponent {
    try_restored_component(f, instance, target, restore).unwrap()
}
pub fn try_restored_component(
    f: &NotebookFixture,
    instance: u64,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let context = HostContext::prepare(
        notes::definition(),
        HostContextInputs {
            records: f.records.clone(),
            views: Rc::new(
                InstanceViewStore::new(f.state_root.clone(), "fixture-device", canonical::ROOM)
                    .unwrap(),
            ),
            device_id: "fixture-device".into(),
            instance_id: instance,
            permissions: f.authority.clone(),
            author: f.author.clone(),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .unwrap();
    notes::definition().instantiate(context, target, restore)
}
pub fn renderer() -> ui::TextRenderer {
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    ui::TextRenderer::with_font_catalog(&catalog).unwrap()
}
/// Mount `editor` in the production frame host `width` wide at its content
/// height, the way a scrolling page lays its body out, with its top-left
/// corner at `(x, y)`, and run its first frame.
pub fn framed_block_editor(
    editor: ui::widget::BlockEditor,
    width: f32,
    (x, y): (f32, f32),
    theme: ui::Theme,
) -> ui::headless::Framed<ui::widget::BlockEditor> {
    // Tall enough that the viewport cull never drops a fixture block.
    const HEIGHT: f32 = 10_000.0;
    ui::headless::Framed::with_options(
        ui::State::new(),
        editor,
        ui::headless::viewport((x + width, HEIGHT)),
        ui::headless::FrameOptions {
            theme: Some(theme),
            text: Some(renderer()),
            host_style: Some(taffy::Style {
                flex_direction: taffy::FlexDirection::Column,
                padding: taffy::Rect {
                    left: taffy::LengthPercentage::length(x),
                    top: taffy::LengthPercentage::length(y),
                    right: taffy::LengthPercentage::length(0.0),
                    bottom: taffy::LengthPercentage::length(0.0),
                },
                ..Default::default()
            }),
            content_height: true,
        },
    )
}
pub fn runner_inputs() -> ui::application::ComponentRunnerInputs {
    ui::application::ComponentRunnerInputs {
        theme: ui::Theme::dark(),
        text_renderer: renderer(),
    }
}
/// Write the fixture's notebook into its record.
pub fn notebook(f: &NotebookFixture) -> ui::record::RecordCheckout {
    f.write_notebook()
}

pub fn assert_closed(report: &ui::application::ComponentCloseReport) {
    assert!(
        report.record_error.is_none(),
        "record close: {:?}",
        report.record_error
    );
    assert!(
        report.view_error.is_none(),
        "view close: {:?}",
        report.view_error
    );
    assert!(
        report.teardown_error.is_none(),
        "teardown close: {:?}",
        report.teardown_error
    );
}
pub fn close_widget(widget: &mut dyn ui::Widget) {
    let reports = ui::component_lifecycle::close_components(widget);
    assert!(!reports.is_empty());
    for report in &reports {
        assert_closed(report);
    }
}

pub fn driver(
    f: &NotebookFixture,
    instance: u64,
    checkout: &RecordCheckout,
) -> ui::headless::Driver {
    let mut driver = ui::headless::Driver::from_component(
        component(f, instance, existing(checkout)),
        (1000, 700),
        runner_inputs(),
    )
    .unwrap();
    // These scenarios spell host shortcuts with Ctrl (Ctrl+Z undo). The
    // driver otherwise follows the build host's conventions, where macOS
    // binds undo to Cmd and a Ctrl+Z would never reach history.
    driver.set_platform_conventions(ui::PlatformConventions::new(ui::Platform::Linux));
    driver
}
pub fn invoke(
    driver: &mut ui::headless::Driver,
    instance: u64,
    resource: impl Into<String>,
    action: impl Into<String>,
    value: Option<&str>,
) {
    let arguments = value.map_or_else(serde_json::Map::new, |value| {
        serde_json::json!({"value": value})
            .as_object()
            .unwrap()
            .clone()
    });
    driver
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: instance,
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            &arguments,
        )
        .unwrap();
    driver.relayout();
}
/// Invoke `action` of `resource` with `arguments`, as an agent does.
pub fn invoke_with(
    driver: &mut ui::headless::Driver,
    instance: u64,
    resource: impl Into<String>,
    action: impl Into<String>,
    arguments: serde_json::Value,
) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    driver
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: instance,
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            &arguments,
        )
        .unwrap();
    driver.relayout();
}

/// Make a top-level note titled `title`.
pub fn new_note(driver: &mut ui::headless::Driver, instance: u64, title: &str) {
    invoke_with(
        driver,
        instance,
        NOTEBOOK,
        "new-note",
        serde_json::json!({ "title": title }),
    );
}

/// Show note `page`, revealing `block` in it when one is named.
pub fn open_note(
    driver: &mut ui::headless::Driver,
    instance: u64,
    page: notes::PageId,
    block: Option<&str>,
) {
    let arguments = match block {
        Some(block) => serde_json::json!({ "page": page, "block": block }),
        None => serde_json::json!({ "page": page }),
    };
    invoke_with(driver, instance, NOTEBOOK, "open-note", arguments);
}

/// Browse the notes tagged `tag`.
pub fn open_tag(driver: &mut ui::headless::Driver, instance: u64, tag: uuid::Uuid) {
    invoke_with(
        driver,
        instance,
        NOTEBOOK,
        "open-tag",
        serde_json::json!({ "tag": tag }),
    );
}

/// Replace the text of `block` of the fixture's child note through window
/// `instance`, as an agent does: one field edit of the note's body.
pub fn replace_text_through(
    driver: &mut ui::headless::Driver,
    instance: u64,
    f: &NotebookFixture,
    checkout: &RecordCheckout,
    block: &str,
    text: &str,
) {
    let old = f
        .records
        .read(&checkout.id, |doc| {
            notes::read::page_block_snapshots(doc, f.child)
                .into_iter()
                .find(|(id, _)| id == block)
                .unwrap()
                .1
        })
        .unwrap();
    let edit = notes::Notebook::paths()
        .pages
        .item(&notes::notebook::page_id(f.child))
        .body
        .perform(&tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: block.to_owned(),
            at: 0,
            del: old.chars().count(),
            ins: text.to_owned(),
        })
        .unwrap();
    invoke_with(
        driver,
        instance,
        NOTEBOOK,
        "field-edit",
        serde_json::json!({ "edit": edit }),
    );
}

/// Give note `page` tag `tag` through window `instance`: one field edit.
pub fn assign_tag_through(
    driver: &mut ui::headless::Driver,
    instance: u64,
    page: notes::PageId,
    tag: uuid::Uuid,
) {
    let edit = notes::Notebook::paths()
        .pages
        .item(&notes::notebook::page_id(page))
        .tags
        .insert(&notes::notebook::page_id(tag));
    invoke_with(
        driver,
        instance,
        NOTEBOOK,
        "field-edit",
        serde_json::json!({ "edit": edit }),
    );
}

/// The notebook's published resource.
pub const NOTEBOOK: &str = "notes.notebook";

pub fn close_driver(driver: &mut ui::headless::Driver) {
    driver.close_components();
    let reports = driver.take_component_close_reports();
    assert!(!reports.is_empty());
    for report in &reports {
        assert_closed(report);
    }
}
/// Apply one rich-text edit to `page`'s body, as a collaborator's edit
/// arrives.
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers build the edit in place"
)]
pub fn edit_body(
    doc: &mut automerge::AutoCommit,
    page: notes::PageId,
    edit: tilo_document::RichTextAction,
) {
    tilo_actions::Action::apply(&notes::actions::edit_body_action(page, edit), doc).unwrap();
}

/// Apply one notebook action to `doc`, as a collaborator's change arrives.
///
/// # Errors
/// The action's refusal.
pub fn apply_to(
    doc: &mut automerge::AutoCommit,
    action: &notes::actions::Action,
) -> Result<(), String> {
    tilo_actions::Action::apply(action, doc).map_err(|error| error.to_string())
}

/// Apply one rich-text edit to `page`'s body.
///
/// # Errors
/// The edit's refusal.
pub fn edit_page_body(
    doc: &mut automerge::AutoCommit,
    page: notes::PageId,
    edit: &tilo_document::RichTextAction,
) -> Result<(), String> {
    apply_to(doc, &notes::actions::edit_body_action(page, edit))
}

/// Write note `page` titled `title` under `parent`, as a collaborator does:
/// made when the notebook lacks it, renamed and moved when it has it.
pub fn upsert_page(doc: &mut automerge::AutoCommit, page: &notes::PageMeta) {
    let exists = notes::read::list_pages(doc)
        .iter()
        .any(|known| known.id == page.id);
    if exists {
        apply_to(
            doc,
            &notes::actions::rename_page_action(page.id, &page.title),
        )
        .unwrap();
        apply_to(
            doc,
            &notes::actions::move_page_action(page.id, page.parent, None),
        )
        .unwrap();
    } else {
        apply_to(
            doc,
            &notes::actions::create_page_action(page.id, page.parent, &page.title),
        )
        .unwrap();
    }
}

/// Copy note `page` and the notes under it; the copy's id.
pub fn duplicate(doc: &mut automerge::AutoCommit, page: notes::PageId) -> notes::PageId {
    let before = notes::read::list_pages(doc);
    let parent = before.iter().find(|known| known.id == page).unwrap().parent;
    apply_to(doc, &notes::actions::duplicate_action(page)).unwrap();
    notes::read::list_pages(doc)
        .into_iter()
        .find(|copy| copy.parent == parent && before.iter().all(|known| known.id != copy.id))
        .unwrap()
        .id
}

/// Replace the whole text of `block`.
pub fn replace_block_text(
    doc: &mut automerge::AutoCommit,
    page: notes::PageId,
    block: &str,
    text: &str,
) {
    let old = notes::read::page_block_snapshots(doc, page)
        .into_iter()
        .find(|(id, _)| id == block)
        .map(|(_, text)| text)
        .unwrap();
    edit_body(
        doc,
        page,
        tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: block.to_owned(),
            at: 0,
            del: old.chars().count(),
            ins: text.to_owned(),
        },
    );
}

/// Remove `block` from `page`.
pub fn remove_block(doc: &mut automerge::AutoCommit, page: notes::PageId, block: &str) {
    edit_body(
        doc,
        page,
        tilo_document::RichTextAction::RemoveBlocks {
            blocks: vec![block.to_owned()],
        },
    );
}

/// The stored spelling of the kind of `page`'s block at `index`.
pub fn kind_at(doc: &automerge::AutoCommit, page: notes::PageId, index: usize) -> String {
    notes::read::page_blocks(doc, page)[index].kind.stored()
}

pub fn revision(f: &NotebookFixture, checkout: &RecordCheckout) -> u64 {
    f.records
        .read_history(&checkout.id, |_, history| history.revision())
        .unwrap()
}
pub fn page_rect(driver: &ui::headless::Driver, page: notes::PageId) -> vello::kurbo::Rect {
    semantic_rect(driver, &format!("notes:page:{page}"))
}
pub fn semantic_rect(driver: &ui::headless::Driver, target: &str) -> vello::kurbo::Rect {
    fn access(node: &ui::a11y::AccessNode, target: &str) -> Option<vello::kurbo::Rect> {
        if node.action_target.as_deref() == Some(target)
            || node.semantic_key.as_deref() == Some(target)
        {
            return node.bounds.map(ui::a11y::AccessBounds::to_rect);
        }
        node.children.iter().find_map(|child| access(child, target))
    }
    fn retained(node: &ui::headless::InspectionNode, target: &str) -> Option<vello::kurbo::Rect> {
        node.semantics
            .as_ref()
            .and_then(|node| access(node, target))
            .or_else(|| node.children.iter().find_map(|node| retained(node, target)))
    }
    retained(&driver.inspection().root, target)
        .or_else(|| driver.targets().get(target).copied())
        .unwrap()
}

/// The actual component boundary with its admitted layout, for an unfinished
/// native typing transaction before another instance mounts.
pub struct BoundNotes {
    tile: Box<ui::widget::TileHost>,
    bindings: ui::bound_dispatch::BoundNodeMap,
    nodes: std::collections::HashMap<taffy::NodeId, *mut dyn ui::Widget>,
    node: taffy::NodeId,
    tree: taffy::TaffyTree<()>,
    outer: automerge::AutoCommit,
}
impl BoundNotes {
    pub fn new(component: AppComponent) -> Self {
        use ui::Widget;
        fn editor(widget: &dyn Widget) -> Option<usize> {
            if widget.test_id() == Some("block_editor") {
                return Some(std::ptr::from_ref(widget).cast::<()>() as usize);
            }
            widget.children().into_iter().find_map(editor)
        }
        let mut tile = Box::new(ui::widget::TileHost::from_component(component));
        // A host syncs a mounted component before laying it out: the
        // notebook surface binds its open note's editor then.
        let outer = automerge::AutoCommit::new();
        tile.sync(&outer, &automerge::ROOT);
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let layout = tile.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
        let pointer = editor(tile.as_ref()).unwrap();
        let matching = nodes
            .iter()
            .filter_map(|(&node, &registered)| {
                (registered.cast::<()>() as usize == pointer).then_some(node)
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: the boxed owner and this exact layout stay stable until close.
        unsafe {
            bindings.rebuild(tile.as_ref(), &tree, layout, &nodes);
        }
        Self {
            tile,
            bindings,
            nodes,
            node: matching[0],
            tree,
            outer,
        }
    }
    pub fn pending_edit(
        &mut self,
        edit: impl FnOnce(&mut automerge::AutoCommit),
    ) -> Result<(), ui::bound_dispatch::BoundDispatchError> {
        // SAFETY: exclusive self retains the same boxed tree and both admission maps.
        unsafe {
            ui::bound_dispatch::invoke_bound_node(
                &self.bindings,
                &self.nodes,
                self.node,
                &mut self.outer,
                &automerge::ROOT,
                |_, doc, _| edit(doc),
            )
        }
    }

    pub fn queue_block_move(&mut self, block: &str) {
        // SAFETY: same owned, admitted editor as pending_edit; only its local
        // semantic intent changes, and Notes drains that intent on its next sync.
        let handled = unsafe {
            ui::bound_dispatch::invoke_bound_node(
                &self.bindings,
                &self.nodes,
                self.node,
                &mut self.outer,
                &automerge::ROOT,
                |widget, doc, root| {
                    widget.perform_semantic_action(
                        &format!("notes.block-move:{block}:up"),
                        &ui::a11y::Action::Activate,
                        &mut Edits::lend(doc),
                        root,
                    )
                },
            )
        }
        .unwrap();
        assert!(handled);
    }

    pub fn into_driver(self, author: ui::permissions::UserId) -> ui::headless::Driver {
        drop(self.bindings);
        drop(self.nodes);
        drop(self.tree);
        ui::headless::Driver::new_with_controlled_inputs(
            ui::state::State::new(),
            self.tile,
            (1000, 700),
            ui::Theme::dark(),
            renderer(),
            author,
        )
    }

    pub fn close(&mut self) {
        close_widget(self.tile.as_mut());
    }
}

pub fn invoke_confirmed(
    driver: &mut ui::headless::Driver,
    instance: u64,
    resource: String,
    action: &str,
) {
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: instance,
        address: tilo_actions::ActionAddress::new(resource, action),
    };
    driver
        .invoke_bound_action_confirmed(&address, &serde_json::Map::new())
        .unwrap();
    driver.relayout();
}

pub fn move_pointer(driver: &mut ui::headless::Driver, point: vello::kurbo::Point) {
    driver.send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
}
pub fn mouse(
    driver: &mut ui::headless::Driver,
    button: winit::event::MouseButton,
    state: winit::event::ElementState,
) {
    driver.send(&winit::event::WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        button,
        state,
    });
}
