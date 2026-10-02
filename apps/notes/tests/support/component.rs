//! Explicit temporary canonical records and controlled runner fonts.
#[path = "canonical.rs"]
pub mod canonical;
pub use canonical::LegacyFixture;
use std::rc::Rc;
use ui::application::{AppComponent, HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::application_view::InstanceViewStore;
use ui::record::RecordCheckout;

pub fn existing(checkout: &RecordCheckout) -> LaunchTarget {
    LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    }
}
pub fn component(f: &LegacyFixture, instance: u64, target: LaunchTarget) -> AppComponent {
    restored_component(f, instance, target, RestoreInput::None)
}
pub fn restored_component(
    f: &LegacyFixture,
    instance: u64,
    target: LaunchTarget,
    restore: RestoreInput,
) -> AppComponent {
    try_restored_component(f, instance, target, restore).unwrap()
}
pub fn try_restored_component(
    f: &LegacyFixture,
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
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/sdk/ui-core/assets/fonts"),
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
pub fn imported(f: &LegacyFixture) -> ui::record::RecordCheckout {
    notes::legacy_import::import_legacy(&f.records, &f.source, f.key)
        .unwrap()
        .checkout
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

pub fn driver(f: &LegacyFixture, instance: u64, checkout: &RecordCheckout) -> ui::headless::Driver {
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
    notes::doc::NotesDoc::edit_page_body(doc, page, &edit).unwrap();
}

/// Replace the whole text of `block`.
pub fn replace_block_text(
    doc: &mut automerge::AutoCommit,
    page: notes::PageId,
    block: &str,
    text: &str,
) {
    let old = notes::doc::NotesDoc::page_block_snapshots(doc, page)
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
    notes::doc::NotesDoc::page_blocks(doc, page)[index]
        .kind
        .stored()
}

pub fn revision(f: &LegacyFixture, checkout: &RecordCheckout) -> u64 {
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
    retained(&driver.inspection().root, target).unwrap()
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
            outer: automerge::AutoCommit::new(),
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
                        doc,
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
