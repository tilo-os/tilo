//! Test helpers for synthesizing shell-level input in unit tests.
//!
//! Winit's `KeyEvent` carries a private `platform_specific` field that
//! can't be filled in from outside the winit crate, so a full
//! `WindowEvent::KeyboardInput` can't be built from test code on Linux.
//! [`KeyPress`] is a tiny constructible stand-in that
//! [`super::Shell::dispatch_key`] consumes directly; the compositor
//! path extracts the same logical-key + modifier pair from a winit event
//! and feeds it through the same dispatcher, so both paths exercise identical
//! routing code.
//!
//! `Mods` is hand-rolled as a `u8` bitmask rather than pulled in via
//! `bitflags`, to keep the crate's dependency surface tight.

/// Logical modifier mask for [`KeyPress`], independent of winit's
/// `ModifiersState`. `CMD` is the Super/Meta "GUI" modifier (Cmd+K,
/// Cmd+Tab); on the Linux dev target Ctrl is an accepted stand-in, both
/// setting the same bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const CMD: Mods = Mods(1 << 0);
    pub const SHIFT: Mods = Mods(1 << 1);
    pub const ALT: Mods = Mods(1 << 2);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: Mods) -> bool {
        (self.0 & other.0) == other.0
    }

    #[must_use]
    pub const fn union(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        self.union(rhs)
    }
}

/// A synthesized key press, routed into [`super::Shell::dispatch_key`].
/// The dispatcher lowercases characters, so `Cmd+K` and `Cmd+k` hit the
/// same branch — tests use uppercase purely for readability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub logical: String,
    pub mods: Mods,
}

/// `logical`: canonical names (`"Escape"`, `"Tab"`, `"ArrowUp"`, …) or any
/// single-character string for a printable character.
pub fn key_press(logical: &str, mods: Mods) -> KeyPress {
    KeyPress {
        logical: logical.to_string(),
        mods,
    }
}

#[cfg(test)]
pub(crate) struct PopulatedShellFixture {
    pub(crate) frame: crate::shell::frame::FrameBuilder,
    pub(crate) parent: tempfile::TempDir,
}

#[cfg(test)]
impl PopulatedShellFixture {
    pub(crate) fn new(viewport: (u32, u32), theme: ui::theme::Theme) -> Self {
        let parent = tempfile::tempdir().expect("private populated shell fixture parent");
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/assets/fonts"),
            "en-US",
        );
        let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog)
            .expect("controlled populated shell fonts must load");
        let inputs =
            crate::shell::fixture_inputs::ShellFixtureInputs::new(parent.path(), renderer, theme)
                .expect("owned populated shell fixture files");
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
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
        document,
        &automerge::ROOT,
        focus,
    );
    let pressed = frame.dispatch_event(
        &press,
        &Modifiers::default(),
        document,
        &automerge::ROOT,
        focus,
    );
    let released = frame.dispatch_event(
        &release,
        &Modifiers::default(),
        document,
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
