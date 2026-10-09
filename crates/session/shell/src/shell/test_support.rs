//! Test helpers for synthesizing shell-level input in unit tests.

#[cfg(test)]
pub(crate) struct PopulatedShellFixture {
    pub(crate) frame: crate::shell::frame::FrameBuilder,
    pub(crate) parent: tempfile::TempDir,
}

#[cfg(test)]
impl PopulatedShellFixture {
    pub(crate) fn new(viewport: (u32, u32), theme: ui::theme::Theme) -> Self {
        Self::with_tree(
            viewport,
            theme,
            crate::shell::state::WorkspaceTree::bootstrap_demo(),
        )
    }

    /// The fixture over `tree` instead of the demo's.
    pub(crate) fn with_tree(
        viewport: (u32, u32),
        theme: ui::theme::Theme,
        tree: crate::shell::state::WorkspaceTree,
    ) -> Self {
        let parent = tempfile::tempdir().expect("private populated shell fixture parent");
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            ui::font_catalog::checkout_root(),
            "en-US",
        );
        let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog)
            .expect("controlled populated shell fonts must load");
        let inputs =
            crate::shell::fixture_inputs::ShellFixtureInputs::new(parent.path(), renderer, theme)
                .expect("owned populated shell fixture files");
        let tab_ids = tree
            .desktop
            .active_workspace()
            .expect("populated fixture has an active workspace")
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        let frame = crate::shell::frame::FrameBuilder::with_fixture_inputs(tree, viewport, inputs)
            .expect("explicit populated shell fixture");
        let records = frame.chrome().records().clone();
        for tab in tab_ids {
            crate::shell::tab_documents::create_scratch_tab_record(&records, tab)
                .expect("populated fixture explicitly creates each presented tab record");
        }
        Self { frame, parent }
    }
}

/// A collaborator renames the active workspace from their own device: the
/// kind of change Catch up and the unread badge exist to show, which the
/// reader's own bootstrap writes are not.
#[cfg(test)]
pub(crate) fn collaborator_renames_active_workspace(tree: &mut crate::shell::state::WorkspaceTree) {
    let reader = tree.doc.get_actor().clone();
    tree.doc
        .set_actor(automerge::ActorId::from(b"ada-device".as_slice()));
    if let Some(workspace) = tree.desktop.active_workspace_mut() {
        workspace.name = "Board review".to_string();
    }
    tree.seed_doc_from_cache();
    tree.doc.set_actor(reader);
}

#[cfg(test)]
impl std::ops::Deref for PopulatedShellFixture {
    type Target = crate::shell::frame::FrameBuilder;

    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}

#[cfg(test)]
impl std::ops::DerefMut for PopulatedShellFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frame
    }
}

#[cfg(test)]
pub(crate) fn dispatch_populated_fixture_click(
    frame: &mut crate::shell::frame::FrameBuilder,
    target: &str,
    document: &mut automerge::AutoCommit,
    focus: &mut Option<ui::taffy::NodeId>,
) -> vello::Scene {
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

    let center = frame
        .widget_registry()
        .get(target)
        .expect("actual populated shell control")
        .center();
    let cursor = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(center.x, center.y),
    };
    let press = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    let release = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    let _ = frame.dispatch_event(
        &cursor,
        &Modifiers::default(),
        &mut ui::widget::Edits::lend(document),
        &automerge::ROOT,
        focus,
    );
    let pressed = frame.dispatch_event(
        &press,
        &Modifiers::default(),
        &mut ui::widget::Edits::lend(document),
        &automerge::ROOT,
        focus,
    );
    let released = frame.dispatch_event(
        &release,
        &Modifiers::default(),
        &mut ui::widget::Edits::lend(document),
        &automerge::ROOT,
        focus,
    );
    assert!(
        pressed || released,
        "populated shell control {target} handles click"
    );
    frame.pump_apps(document, &automerge::ROOT);
    frame.relayout();
    frame.paint()
}

/// Run `f` with a throwaway event context: an empty document, no focus
/// target and a one-node layout tree.
#[cfg(test)]
pub(crate) fn with_scratch_event_ctx<R>(f: impl FnOnce(&mut ui::widget::EventCtx<'_>) -> R) -> R {
    let mut taffy = ui::taffy::TaffyTree::<()>::new();
    let node = taffy
        .new_leaf(ui::taffy::Style::default())
        .expect("a leaf in an empty tree");
    let mut document = automerge::AutoCommit::new();
    let modifiers = winit::event::Modifiers::default();
    let mut handled = false;
    let mut focus = None;
    f(&mut ui::widget::EventCtx {
        scale_factor: 1.0,
        modifiers: &modifiers,
        state: ui::widget::Edits::lend(&mut document),
        path: &automerge::ROOT,
        handled: &mut handled,
        focus_target: &mut focus,
        node_id: node,
        taffy: &taffy,
    })
}

#[cfg(test)]
pub(crate) use ambient::{DenyAmbientStarts, ambient_start};

#[cfg(test)]
mod ambient {

    static DENY_AMBIENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static AMBIENT_ATTEMPTS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    pub(crate) struct DenyAmbientStarts {
        initial_attempts: usize,
    }
    impl DenyAmbientStarts {
        pub(crate) fn new() -> Self {
            use std::sync::atomic::Ordering;
            assert!(
                !DENY_AMBIENT.swap(true, Ordering::SeqCst),
                "ambient guard already armed"
            );
            AMBIENT_ATTEMPTS.store(0, Ordering::SeqCst);
            Self {
                initial_attempts: 0,
            }
        }
        pub(crate) fn attempts(&self) -> usize {
            AMBIENT_ATTEMPTS
                .load(std::sync::atomic::Ordering::SeqCst)
                .saturating_sub(self.initial_attempts)
        }
    }
    impl Drop for DenyAmbientStarts {
        fn drop(&mut self) {
            DENY_AMBIENT.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    pub(crate) fn ambient_start(boundary: &'static str) {
        use std::sync::atomic::Ordering;
        if DENY_AMBIENT.load(Ordering::SeqCst) {
            AMBIENT_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
            panic!("ambient service start forbidden: {boundary}");
        }
    }
}
