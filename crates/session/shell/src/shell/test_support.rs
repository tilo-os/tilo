//! Test helpers for synthesizing shell-level input in unit tests.

/// A canonical package manifest for an installed-app fixture: `package` is
/// the body of `[package]`, `tail` the tables after the sandbox
/// (`[entrypoint]`, `[providers]`, ...). Manifests are decoded through the
/// canonical schema, so a fixture carries its publisher and sandbox too;
/// `[sandbox.net]` is `off` unless `tail` sets it.
#[cfg(test)]
pub(crate) fn canonical_manifest(package: &str, tail: &str) -> String {
    let net = if tail.contains("[sandbox.net]") {
        ""
    } else {
        "[sandbox.net]\nmode = \"off\"\n"
    };
    format!(
        "[package]\n{package}\n[publisher]\nidentity = \"ed25519:{}\"\n\
         [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         {net}{tail}",
        "00".repeat(32)
    )
}

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
    use winit::event::Modifiers;

    let center = frame
        .widget_registry()
        .get(target)
        .expect("actual populated shell control")
        .center();
    let cursor = ui::synthetic_input::cursor_moved(center.x, center.y);
    let press = ui::synthetic_input::mouse_press();
    let release = ui::synthetic_input::mouse_release();
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

/// When pan step `step` of a fixture that does not model input timing was
/// reported: well before the frame and far apart from the step before, so
/// the strip's frame-time resampling has nothing to interpolate and each
/// frame shows exactly the travel last reported, and a release throws
/// nothing.
#[cfg(test)]
pub(crate) fn settled_pan_report(step: u32) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_sub(std::time::Duration::from_secs(30))
        .and_then(|base| {
            base.checked_add(std::time::Duration::from_millis(50).saturating_mul(step))
        })
        .unwrap_or(now)
}

#[cfg(test)]
pub(crate) use ambient::{DenyAmbientStarts, ambient_start};

/// This process's settings document, replaced by `source` until the guard
/// drops and then restored, also while a failing test unwinds.
///
/// Which document the process reads is as process-wide as its environment:
/// the installed source, else the file `XDG_CONFIG_HOME` names. A test that
/// installed a source beside a test holding a [`tilo_test_env::TestHome`]
/// took that test's settings writes, and uninstalling it sent the other
/// test's reads back to a file it never wrote: the automatic display input
/// tests saw their own policy vanish in a parallel run. So the guard holds
/// the process-wide test-environment lock, as `TestHome` does, for as long
/// as the source is installed.
#[cfg(test)]
#[must_use = "the source is uninstalled as soon as the guard drops"]
pub(crate) struct InstalledSettings {
    previous: Option<std::sync::Arc<dyn settings_store::SettingsSource>>,
    // Released after `drop` restored the previous source.
    _env: tilo_test_env::EnvLock,
}

/// Install `source` as this process's settings until the guard drops.
#[cfg(test)]
pub(crate) fn install_settings(
    source: std::sync::Arc<dyn settings_store::SettingsSource>,
) -> InstalledSettings {
    let env = tilo_test_env::lock();
    let previous = settings_store::installed_source();
    settings_store::install_source(Some(source));
    InstalledSettings {
        previous,
        _env: env,
    }
}

#[cfg(test)]
impl Drop for InstalledSettings {
    fn drop(&mut self) {
        settings_store::install_source(self.previous.take());
    }
}

/// Forbids production service starts (workers, process-global handles,
/// ambient identity and file lookups) for the lifetime of a guard.
///
/// The guard is scoped to the thread that arms it, which is the test that
/// owns it: libtest runs each test on its own thread, so a guard in one
/// test never trips the production paths another test exercises
/// concurrently. Every `ambient_start` boundary runs synchronously on the
/// thread that requests the service, before any worker is spawned, so a
/// thread-local guard still sees every start its own test makes.
#[cfg(test)]
mod ambient {
    use std::cell::Cell;

    thread_local! {
        static DENY_AMBIENT: Cell<bool> = const { Cell::new(false) };
        static AMBIENT_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) struct DenyAmbientStarts {
        /// This thread's attempt count when the guard was armed.
        initial_attempts: usize,
        /// Thread-bound: the guard only covers the thread that armed it.
        _not_send: std::marker::PhantomData<*const ()>,
    }
    impl DenyAmbientStarts {
        pub(crate) fn new() -> Self {
            assert!(
                !DENY_AMBIENT.replace(true),
                "ambient guard already armed on this test thread"
            );
            Self {
                initial_attempts: AMBIENT_ATTEMPTS.get(),
                _not_send: std::marker::PhantomData,
            }
        }
        pub(crate) fn attempts(&self) -> usize {
            AMBIENT_ATTEMPTS.get().saturating_sub(self.initial_attempts)
        }
    }
    impl Drop for DenyAmbientStarts {
        fn drop(&mut self) {
            DENY_AMBIENT.set(false);
        }
    }
    pub(crate) fn ambient_start(boundary: &'static str) {
        if DENY_AMBIENT.get() {
            AMBIENT_ATTEMPTS.set(AMBIENT_ATTEMPTS.get().saturating_add(1));
            panic!("ambient service start forbidden: {boundary}");
        }
    }

    #[test]
    fn a_guard_forbids_only_its_own_tests_starts() {
        let guard = DenyAmbientStarts::new();
        let concurrent_test = std::thread::spawn(|| {
            ambient_start("a concurrent test's production path");
            let own = DenyAmbientStarts::new();
            assert_eq!(own.attempts(), 0);
        });
        assert!(
            concurrent_test.join().is_ok(),
            "another test's guard tripped a concurrent test"
        );
        let own_start = std::panic::catch_unwind(|| ambient_start("own production path"));
        assert!(own_start.is_err());
        assert_eq!(guard.attempts(), 1);
    }
}

