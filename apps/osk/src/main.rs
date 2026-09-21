//! Thin binary entry point for the Tilo on-screen keyboard.
//!
//! Two modes, mirroring the other `ui` apps (e.g. file-explorer):
//!   * default — build an [`OskWidget`], hand it to `ui::App`, run the live
//!     winit event loop.
//!   * `--shots <PATH>` — render the keyboard at a default size into a PNG at
//!     `<PATH>` and exit. No window, no event loop. Used for headless
//!     screenshot tests and CI snapshots.
//!
//! ## Tilo integration
//!
//! The session compositor supervises this signed service when the persisted
//! accessibility preference is enabled. It gives the service a dedicated,
//! unexported Wayland socket. Only connections accepted on that socket can see
//! `input-method-v2` or `virtual-keyboard-v1`; copying the app-id on the public
//! display grants no input authority.
//!
//! The main xdg-toplevel announces the stable [`osk::KEYBOARD_APP_ID`]. The
//! compositor requires both that id and the private-socket credential before it
//! promotes the surface out of the workspace tree into a bottom floating
//! overlay. Input-method activation makes the initially empty keyboard visible;
//! leaving the focused text field suppresses it again. Pointer focus may enter
//! the keyboard, but its taps deliberately do not take keyboard focus from the
//! edited field.
//!
//! Typed and swiped text follows the protocol-clean `commit_string` path. A
//! narrowly gated virtual keyboard handles Backspace because winit's current
//! `text-input-v3` client does not consume `delete_surrounding_text`. On other
//! compositors, the binary may use its standalone virtual-keyboard fallback;
//! Tilo never advertises that privileged global on its public display.
//!
//! Swipe decoding and prediction are live: the real FUTO model is selected
//! when its runtime assets are available, otherwise the in-tree geometric
//! decoder provides the same candidate UI.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::sync::{Arc, Mutex};

use osk::InputPurpose;
use osk::OskWidget;
use osk::imclient::{ImState, InputMethodClient, try_input_method};
use osk::inject::default_injector;
use osk::input_purpose_name;
use ui::app::UserEvent;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

/// Default offscreen viewport for `--shots`. Phone-portrait-ish width with
/// just enough height for the toolbar + suggestion bar + four key rows
/// (48 + 40 + 4×56 = 312, rounded up).
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (720, 360);

/// # Errors
///
/// Returns an error when `--input-method` is explicitly requested but the
/// compositor does not advertise `zwp_input_method_manager_v2`, or when
/// `--shots` fails to render (taffy layout failure, e.g. the tree exceeds its
/// node capacity).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&launch_spec());
}

fn launch_spec() -> LaunchSpec {
    launch_spec_with_root(shot_root)
}

fn launch_spec_with_root(
    root: fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>,
) -> LaunchSpec {
    LaunchSpec::new_with_args("so.tilo.osk", root)
        .history(&osk::framework_history::HISTORY)
        .shot_root(shot_root)
        .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
        .allowed_live_flags(&["--input-method", "--no-input-method"])
        .allowed_live_options(&["--purpose"])
        .disable_headless()
        .live_handler(run_live)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec's argument-aware root factory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(shot_root_with_model(
        args,
        osk::widget::default_swipe_model(),
    ))
}

fn shot_root_with_model(args: &LaunchArgs, model: Box<dyn osk::SwipeModel>) -> Box<dyn Widget> {
    let mut widget = OskWidget::new_with_swipe_model(model);
    if let Some(purpose) = parse_purpose_arg(args) {
        widget.set_purpose(purpose);
    }
    Box::new(widget)
}

fn run_live(args: &LaunchArgs) -> Result<(), LaunchError> {
    let purpose = parse_purpose_arg(args);

    // --- pick the input path. Prefer auto-show via input-method-v2 when the
    // compositor supports it: the keyboard appears on text focus, follows the
    // field's content purpose, hides on blur, and commits text via the IM. Fall
    // back to the always-visible keyboard + virtual-keyboard injection
    // otherwise. ---
    let im_pref = parse_input_method_pref(args);
    let im = match im_pref {
        // Force the always-visible path; never try the input method.
        InputMethodPref::Off => None,
        // Try; fail loudly if explicitly requested but unavailable.
        InputMethodPref::Require => match try_input_method() {
            Some(im) => Some(im),
            None => {
                return Err(LaunchError::Operation(
                    "osk: --input-method requested but the compositor does not advertise \
                     zwp_input_method_manager_v2"
                        .into(),
                ));
            }
        },
        // Default: use it if present, otherwise degrade silently.
        InputMethodPref::Auto => try_input_method(),
    };

    if let Some(im) = im {
        tracing::info!("osk: auto-show via input-method-v2");
        run_auto_show(args, im, purpose)?;
    } else {
        run_always_visible(args, purpose)?;
    }
    Ok(())
}

/// The always-visible keyboard: it fills its surface and types into the focused
/// app via `virtual-keyboard-v1` (or a no-op when that protocol is absent). This
/// is the path when there is no input method to drive auto-show.
fn run_always_visible(args: &LaunchArgs, purpose: Option<InputPurpose>) -> Result<(), LaunchError> {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    let mut injector = default_injector();
    tracing::info!("osk: text-injection backend = {}", injector.backend_name());
    let widget = OskWidget::new().on_key(move |action| {
        injector.key_action(&action);
    });
    // Production uses a wlr-layer-shell overlay; this launch path uses a plain
    // `ui::App` window. The prepared builder applies the initial purpose.
    // Deliberately no `--headless-drive` hook. Every key this widget commits
    // is forwarded to `injector`, which types into whatever application holds
    // focus in the live session — driving it from a test harness would inject
    // keystrokes into the user's desktop.

    let app = build_always_visible_app(
        launch_spec(),
        args,
        wake_scope,
        widget,
        purpose,
        LaunchSpec::build_prepared_root,
    )?;
    app.run();
    Ok(())
}

// Both production modes finish their builders before selecting an immutable
// instance-matched declaration and transferring this exact root to the runner.
fn build_always_visible_app(
    spec: LaunchSpec,
    args: &LaunchArgs,
    wake_scope: ui::WakeScope,
    mut widget: OskWidget,
    purpose: Option<InputPurpose>,
    build: impl FnOnce(
        &LaunchSpec,
        &LaunchArgs,
        ui::WakeScope,
        Box<dyn Widget>,
    ) -> Result<ui::App, LaunchError>,
) -> Result<ui::App, LaunchError> {
    if let Some(purpose) = purpose {
        widget.set_purpose(purpose);
    }
    finish_prepared_app(spec, args, wake_scope, widget, build)
}
fn build_auto_show_app(
    spec: LaunchSpec,
    args: &LaunchArgs,
    wake_scope: ui::WakeScope,
    widget: OskWidget,
    purpose: Option<InputPurpose>,
    build: impl FnOnce(
        &LaunchSpec,
        &LaunchArgs,
        ui::WakeScope,
        Box<dyn Widget>,
    ) -> Result<ui::App, LaunchError>,
) -> Result<ui::App, LaunchError> {
    let mut widget = widget.hidden();
    if let Some(purpose) = purpose {
        widget.set_purpose(purpose);
    }
    finish_prepared_app(spec, args, wake_scope, widget, build)
}
fn finish_prepared_app(
    spec: LaunchSpec,
    args: &LaunchArgs,
    wake_scope: ui::WakeScope,
    widget: OskWidget,
    build: impl FnOnce(
        &LaunchSpec,
        &LaunchArgs,
        ui::WakeScope,
        Box<dyn Widget>,
    ) -> Result<ui::App, LaunchError>,
) -> Result<ui::App, LaunchError> {
    let definition = widget
        .document_history_definition()
        .ok_or_else(|| LaunchError::Operation("OSK preferences History is unavailable".into()))?;
    let mut app = build(
        &spec.history(definition),
        args,
        wake_scope,
        Box::new(widget),
    )?
    .with_window_app_id(osk::KEYBOARD_APP_ID);
    osk::actions::register(app.action_registry_mut());
    Ok(app)
}

/// The auto-show keyboard, driven by an `input-method-v2` client.
///
/// The keyboard starts hidden. A background thread non-blockingly pumps the
/// input-method connection; each focus event is folded through an [`ImState`]
/// reducer into a minimal show/hide/purpose command, which it sends to the
/// winit loop via the `EventLoopProxy` as a [`UserEvent::External`] string the
/// widget understands (`osk:show`, `osk:hide`, `osk:purpose:<name>`).
/// Typed/swiped text is committed back to the focused app via the same input
/// method (`commit_string`).
fn run_auto_show(
    args: &LaunchArgs,
    im: InputMethodClient,
    purpose: Option<InputPurpose>,
) -> Result<(), LaunchError> {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    // The IM client is shared: the winit thread's `on_key` callback commits
    // typed text through it, while the background thread reads focus events.
    let im = Arc::new(Mutex::new(im));

    let im_for_keys = Arc::clone(&im);
    // `text-input-v3` in winit intentionally does not implement
    // delete_surrounding_text yet. Keep the compositor-gated virtual keyboard
    // only for non-inserting editing keys such as Backspace; printable text
    // stays on the input-method commit path.
    let mut editing_keys = default_injector();
    let widget = OskWidget::new().on_key(move |action| {
        if matches!(action, osk::KeyAction::Backspace) {
            editing_keys.key_action(&action);
            return;
        }
        // Turn the committed action into text and send it via the input method.
        // State-only actions (shift/symbols/layout) and Backspace produce no
        // commit string; the guarded virtual keyboard path above handles the
        // latter until winit consumes delete_surrounding_text.
        if let Some(text) = osk::inject::action_symbols(&action)
            .into_iter()
            .map(symbol_to_commit_text)
            .collect::<Option<String>>()
            && !text.is_empty()
            && let Ok(mut im) = im_for_keys.lock()
        {
            im.commit_text(&text);
        }
    });
    // Initial purpose is still replaced by the next current IM focus event.
    let app = build_auto_show_app(
        launch_spec(),
        args,
        wake_scope,
        widget,
        purpose,
        LaunchSpec::build_prepared_root,
    )?;

    // Spawn the input-method reader once the event loop proxy exists, so it can
    // push show/hide/purpose commands into the loop.
    let im_for_reader = Arc::clone(&im);
    app.run_with_setup(move |proxy| {
        std::thread::spawn(move || {
            let mut model = ImState::new();
            loop {
                // Poll without blocking, then release the lock immediately so the
                // winit thread's `on_key` callback can take it to commit typed
                // text. (Blocking on the socket while holding the lock would
                // deadlock against that callback.) Focus changes are rare and
                // not latency-critical, so a short sleep between polls is fine.
                let events = {
                    let Ok(mut im) = im_for_reader.lock() else {
                        break;
                    };
                    im.poll()
                };
                for ev in events {
                    let t = model.reduce(ev);
                    // Purpose first, so the first mapped frame already has
                    // the correct layout instead of flashing the text layout.
                    if let Some(p) = t.purpose_changed {
                        let cmd = format!("osk:purpose:{}", input_purpose_name(p));
                        if proxy.send_event(UserEvent::External(cmd)).is_err() {
                            return;
                        }
                    }
                    if let Some(visible) = t.visibility_changed {
                        let cmd = if visible { "osk:show" } else { "osk:hide" };
                        if proxy
                            .send_event(UserEvent::External(cmd.to_string()))
                            .is_err()
                        {
                            return; // event loop gone
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        });
    });
    Ok(())
}

/// The text a single [`osk::Symbol`] inserts when committed through the input
/// method, or `None` for symbols that aren't a plain inserted character (so the
/// whole commit is skipped — e.g. Backspace, handled by the guarded virtual
/// keyboard fallback).
fn symbol_to_commit_text(sym: osk::Symbol) -> Option<char> {
    match sym {
        osk::Symbol::Unicode(c) => Some(c),
        osk::Symbol::Space => Some(' '),
        osk::Symbol::Return => Some('\n'),
        osk::Symbol::Tab => Some('\t'),
        osk::Symbol::BackSpace => None,
    }
}

/// How the binary should treat the input method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMethodPref {
    /// Use the input method if the compositor offers it, else fall back.
    Auto,
    /// Require it — error out if the compositor doesn't offer it.
    Require,
    /// Never use it — always-visible keyboard.
    Off,
}

/// Read the input-method preference from the args. `--no-input-method` forces it
/// off; `--input-method` requires it; absence means auto-detect.
fn parse_input_method_pref(args: &LaunchArgs) -> InputMethodPref {
    if args.has("--no-input-method") {
        InputMethodPref::Off
    } else if args.has("--input-method") {
        InputMethodPref::Require
    } else {
        InputMethodPref::Auto
    }
}

/// Returns the [`InputPurpose`] requested by `--purpose <kind>` (or
/// `--purpose=<kind>`), or `None` when absent. An unrecognised kind is ignored
/// (the keyboard falls back to plain text) so a typo doesn't abort launch.
fn parse_purpose_arg(args: &LaunchArgs) -> Option<InputPurpose> {
    args.value("--purpose").and_then(purpose_from_str)
}

/// Map a `--purpose` value to an [`InputPurpose`]. Accepts both the short
/// spellings a user is likely to type and the enum-ish names.
fn purpose_from_str(s: &str) -> Option<InputPurpose> {
    Some(match s.to_ascii_lowercase().as_str() {
        "text" => InputPurpose::Text,
        "number" | "numeric" => InputPurpose::Number,
        "decimal" => InputPurpose::Decimal,
        "phone" | "tel" | "telephone" => InputPurpose::Telephone,
        "url" => InputPurpose::Url,
        "email" => InputPurpose::Email,
        "pin" => InputPurpose::Pin,
        "password" | "pass" => InputPurpose::Password,
        "date" => InputPurpose::Date,
        "time" => InputPurpose::Time,
        _ => return None,
    })
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    #[test]
    fn framework_history_canonical_launch_declares_preferences() {
        launch_spec()
            .validate_history_declaration()
            .expect("canonical OSK launch must install preferences History");
    }
    use automerge::{ReadDoc, transaction::Transactable};
    use std::{
        cell::{Cell, RefCell},
        path::PathBuf,
        rc::Rc,
    };
    thread_local! {
        static ARGS:RefCell<Option<LaunchArgs>>=const {RefCell::new(None)};
        static DIRECTORY:RefCell<Option<PathBuf>>=const {RefCell::new(None)};
        static OPENS:Cell<usize>=const {Cell::new(0)};
        static FACTORIES:Cell<usize>=const {Cell::new(0)};
    }
    fn capture(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        ARGS.with_borrow_mut(|slot| *slot = Some(args.clone()));
        Ok(Box::new(ui::widget::Text::new("args")))
    }
    fn forbidden(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        FACTORIES.set(FACTORIES.get() + 1);
        Err(LaunchError::Operation("factory must not run".into()))
    }
    fn memory(_: &'static str) -> (ui::State, ui::persist::Autosave) {
        (
            ui::State::new(),
            ui::persist::Autosave::disabled("controlled args"),
        )
    }
    fn opener(app: &'static str) -> (ui::State, ui::persist::Autosave) {
        assert_eq!(app, "so.tilo.osk");
        OPENS.set(OPENS.get() + 1);
        let mut store = ui::persist::DocStore::open_canonical_at(
            app,
            &DIRECTORY.with_borrow(|p| p.clone().unwrap()),
        )
        .unwrap();
        let mut doc = store.load_or_new();
        let mut saver = ui::persist::Autosave::spawn(store, std::time::Duration::from_millis(1));
        saver.seed(&mut doc);
        (ui::State::from_doc(doc), saver)
    }
    #[test]
    fn framework_history_actual_both_prepared_modes_share_durable_owner_and_exact_root_definition()
    {
        drop(
            LaunchSpec::new_with_args("dev.tilo.osk-args", capture)
                .open_state(memory)
                .build_controlled_app_for_test()
                .unwrap(),
        );
        let args = ARGS.with_borrow_mut(|a| a.take().unwrap());
        for auto in [false, true] {
            for (q, a) in [(true, true), (true, false), (false, true), (false, false)] {
                let directory = tempfile::tempdir().unwrap();
                DIRECTORY.with_borrow_mut(|p| *p = Some(directory.path().to_owned()));
                {
                    let mut store =
                        ui::persist::DocStore::open_canonical_at("dev.tilo.osk", directory.path())
                            .unwrap();
                    let mut doc = store.load_or_new();
                    doc.put(automerge::ROOT, "osk.preferred_layout", "azerty")
                        .unwrap();
                    doc.put(automerge::ROOT, "untouched", "retained").unwrap();
                    doc.commit();
                    store.save(&mut doc).unwrap();
                }
                OPENS.set(0);
                FACTORIES.set(0);
                let calls = Rc::new(Cell::new(0));
                let count = calls.clone();
                let mut root =
                    OskWidget::new_with_swipe_model(Box::new(osk::GeometricSwipeModel::english()))
                        .on_key(move |_| count.set(count.get() + 1));
                for (name, canonical) in [("qwerty", q), ("azerty", a)] {
                    if !canonical {
                        let mut layout = osk::KeyboardLayout::by_name(name).unwrap();
                        layout.rows[0][0].label = "custom".into();
                        root = root.with_layout(layout);
                    }
                }
                let expected = root.document_history_definition().unwrap();
                let scope = ui::WakeScope::new();
                let _entry = scope.enter();
                let spec = launch_spec_with_root(forbidden).open_app_global_state(opener);
                let build = |spec: &LaunchSpec,
                             args: &LaunchArgs,
                             scope: ui::WakeScope,
                             root: Box<dyn Widget>| {
                    assert!(std::ptr::eq(
                        root.document_history_definition().unwrap(),
                        expected
                    ));
                    assert_eq!(root.accessibility().is_none(), auto);
                    spec.build_controlled_prepared_root_for_test(args, scope, root)
                };
                let mut app = if auto {
                    build_auto_show_app(spec, &args, scope, root, Some(InputPurpose::Number), build)
                } else {
                    build_always_visible_app(
                        spec,
                        &args,
                        scope,
                        root,
                        Some(InputPurpose::Number),
                        build,
                    )
                }
                .unwrap();
                let (installed, actual) = app.controlled_history_definitions_for_test();
                assert!(std::ptr::eq(installed.unwrap(), expected));
                assert!(std::ptr::eq(actual.unwrap(), expected));
                assert_eq!(OPENS.get(), 1);
                assert_eq!(FACTORIES.get(), 0);
                assert_eq!(Rc::strong_count(&calls), 2);
                assert_eq!(calls.get(), 0);
                assert_eq!(
                    app.state_mut_for_test()
                        .doc
                        .get(automerge::ROOT, "osk.preferred_layout")
                        .unwrap()
                        .unwrap()
                        .0
                        .to_str(),
                    Some("azerty")
                );
                let probe =
                    OskWidget::new_with_swipe_model(Box::new(osk::GeometricSwipeModel::english()));
                let encoded = ui::tilo_actions::door::encode(
                    &probe.resources(&automerge::AutoCommit::new()),
                    &ui::tilo_actions::ActionAddress::new("osk.keyboard.suggestions", "disable"),
                    None,
                )
                .unwrap();
                assert!(app.action_registry_mut().decode(&encoded).is_ok());
                drop(app);
                assert_eq!(Rc::strong_count(&calls), 1);
                assert_eq!(calls.get(), 0);
                let mut store =
                    ui::persist::DocStore::open_canonical_at("dev.tilo.osk", directory.path())
                        .unwrap();
                let doc = store.load_or_new();
                assert_eq!(
                    doc.get(automerge::ROOT, "osk.preferred_layout")
                        .unwrap()
                        .unwrap()
                        .0
                        .to_str(),
                    Some("azerty")
                );
                assert_eq!(
                    doc.get(automerge::ROOT, "untouched")
                        .unwrap()
                        .unwrap()
                        .0
                        .to_str(),
                    Some("retained")
                );
            }
        }
    }
    #[test]
    fn framework_history_canonical_capture_factory_matches_declaration() {
        drop(
            LaunchSpec::new_with_args("dev.tilo.osk-args", capture)
                .open_state(memory)
                .build_controlled_app_for_test()
                .unwrap(),
        );
        let args = ARGS.with_borrow_mut(|a| a.take().unwrap());
        let root = shot_root_with_model(&args, Box::new(osk::GeometricSwipeModel::english()));
        assert!(std::ptr::eq(
            root.document_history_definition().unwrap(),
            &osk::framework_history::HISTORY
        ));
        assert!(root.history_registration().unwrap().is_none());
        launch_spec().validate_history_declaration().unwrap();
    }
    struct NativeObservedRoot {
        root: Box<dyn Widget>,
        takes: Rc<Cell<usize>>,
        acks: Rc<Cell<usize>>,
    }
    impl Widget for NativeObservedRoot {
        fn document_history_definition(
            &self,
        ) -> Option<&'static ui::history_mode::HistoryDefinition> {
            self.root.document_history_definition()
        }
        fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
            self.root.history_restore_readiness(scope)
        }
        fn resources(&self, doc: &automerge::AutoCommit) -> Vec<ui::tilo_actions::Resource> {
            self.root.resources(doc)
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            self.root.local_resource_operation_handlers()
        }
        fn invoke_resource_operation(
            &mut self,
            kind: ui::tilo_actions::OperationKind,
            type_id: &str,
            payload: &[u8],
            doc: &automerge::AutoCommit,
            path: &automerge::ObjId,
        ) -> Result<bool, String> {
            self.root
                .invoke_resource_operation(kind, type_id, payload, doc, path)
        }
        fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
            let value = self.root.take_control_invocation();
            if value.is_some() {
                self.takes.set(self.takes.get() + 1);
            }
            value
        }
        fn document_action_applied(
            &mut self,
            action: &ui::tilo_actions::EncodedAction,
            before: &[automerge::ChangeHash],
            doc: &automerge::AutoCommit,
            path: &automerge::ObjId,
        ) {
            self.acks.set(self.acks.get() + 1);
            self.root.document_action_applied(action, before, doc, path);
        }
        fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
            self.root.sync(doc, path);
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            theme: &ui::Theme,
            map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.root.build_layout(tree, theme, map)
        }
        fn apply_layout(
            &mut self,
            tree: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.root.apply_layout(tree, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut ui::TextRenderer,
            theme: &ui::Theme,
            presence: &ui::presence::PresenceCtx,
        ) {
            self.root.draw(scene, transform, text, theme, presence);
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            scale: f64,
            modifiers: &winit::event::Modifiers,
            doc: &mut automerge::AutoCommit,
            path: &automerge::ObjId,
            handled: &mut bool,
            focused: &mut Option<taffy::NodeId>,
            node: taffy::NodeId,
            tree: &taffy::TaffyTree<()>,
        ) {
            self.root.handle_event(
                event, scale, modifiers, doc, path, handled, focused, node, tree,
            );
        }
        fn name(&self) -> &str {
            self.root.name()
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.root.get_rect()
        }
        fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
            self.root.accessibility()
        }
        fn take_structure_dirty(&mut self) -> bool {
            self.root.take_structure_dirty()
        }
    }
    thread_local! { static NATIVE_INITIAL: Cell<bool> = const { Cell::new(true) }; }
    fn native_state(_: &'static str) -> (ui::State, ui::persist::Autosave) {
        let mut state = ui::State::new();
        state
            .doc
            .put(
                automerge::ROOT,
                "osk.suggestions_enabled",
                NATIVE_INITIAL.get(),
            )
            .unwrap();
        state.commit_action(None, None);
        (
            state,
            ui::persist::Autosave::disabled("controlled native progression"),
        )
    }
    #[test]
    fn framework_history_native_both_modes_return_live_rebuild_idle_settles_once_without_input() {
        drop(
            LaunchSpec::new_with_args("dev.tilo.osk-args", capture)
                .open_state(memory)
                .build_controlled_app_for_test()
                .unwrap(),
        );
        let args = ARGS.with_borrow_mut(|a| a.take().unwrap());
        for auto in [false, true] {
            for (initial, refuse_first) in [(true, false), (false, false), (true, true)] {
                NATIVE_INITIAL.set(initial);
                let acks = Rc::new(Cell::new(0));
                let takes = Rc::new(Cell::new(0));
                let keys = Rc::new(Cell::new(0));
                let key_count = keys.clone();
                let mut root =
                    OskWidget::new_with_swipe_model(Box::new(osk::GeometricSwipeModel::english()))
                        .on_key(move |_| key_count.set(key_count.get() + 1));
                root.set_suggestions_enabled(false);
                let scope = ui::WakeScope::new();
                let _entry = scope.enter();
                let spec = launch_spec_with_root(forbidden).open_app_global_state(native_state);
                let build = |spec: &LaunchSpec,
                             args: &LaunchArgs,
                             scope: ui::WakeScope,
                             root: Box<dyn Widget>| {
                    spec.build_controlled_prepared_root_for_test(
                        args,
                        scope,
                        Box::new(NativeObservedRoot {
                            root,
                            takes: takes.clone(),
                            acks: acks.clone(),
                        }),
                    )
                };
                let mut app = if auto {
                    build_auto_show_app(spec, &args, scope, root, None, build)
                } else {
                    build_always_visible_app(spec, &args, scope, root, None, build)
                }
                .unwrap();
                let heads = app.state_mut_for_test().doc.get_heads();
                let open = app
                    .build_a11y_tree_update(1.0)
                    .nodes
                    .into_iter()
                    .find(|(_, node)| node.label() == Some("History"))
                    .expect("exported native History entry")
                    .0;
                app.dispatch_a11y_action(open, &ui::a11y::Action::Activate);
                app.rebuild_structure();
                assert!(
                    takes.get() >= 1,
                    "opening History must causally discard an already prepared offer"
                );
                assert_eq!(acks.get(), 0);
                app.controlled_local_idle_for_test();
                assert_eq!(app.state_mut_for_test().doc.get_heads(), heads);
                assert_eq!(acks.get(), 0);
                assert_eq!(keys.get(), 0);
                let version = app
                    .build_a11y_tree_update(1.0)
                    .nodes
                    .into_iter()
                    .find(|(_, node)| {
                        node.label()
                            .is_some_and(|label| label.ends_with(": 1 field"))
                    })
                    .expect("exported recorded preference version")
                    .0;
                app.dispatch_a11y_action(version, &ui::a11y::Action::Activate);
                app.rebuild_structure();
                assert_eq!(acks.get(), 0);
                assert_eq!(app.state_mut_for_test().doc.get_heads(), heads);
                let back = app
                    .build_a11y_tree_update(1.0)
                    .nodes
                    .into_iter()
                    .find(|(_, node)| node.label() == Some("Back to live"))
                    .expect("exported native return-live target")
                    .0;
                app.dispatch_a11y_action(back, &ui::a11y::Action::Activate);
                app.rebuild_structure();
                if refuse_first {
                    *app.action_registry_mut() = ui::tilo_actions::ActionRegistry::new();
                    assert!(!app.controlled_local_idle_for_test());
                    assert_eq!(acks.get(), 0);
                    assert_eq!(app.state_mut_for_test().doc.get_heads(), heads);
                    osk::actions::register(app.action_registry_mut());
                    app.rebuild_structure();
                }
                let taken_before_idle = takes.get();
                let idle = app.controlled_local_idle_for_test();
                assert!(
                    idle,
                    "ordinary native idle dispatches the post-discard sync offer: auto={auto},initial={initial},refusal={refuse_first},takes={taken_before_idle}->{},acks={},nodes={:?}",
                    takes.get(),
                    acks.get(),
                    app.build_a11y_tree_update(1.0)
                        .nodes
                        .iter()
                        .filter_map(|(_, n)| n.label())
                        .collect::<Vec<_>>()
                );
                assert_eq!(acks.get(), 1);
                assert_eq!(keys.get(), 0);
                let settled = app.state_mut_for_test().doc.get_heads();
                assert_eq!(
                    settled == heads,
                    !initial,
                    "no-op must add no history boundary"
                );
                assert_eq!(
                    app.state_mut_for_test()
                        .doc
                        .get(automerge::ROOT, "osk.suggestions_enabled")
                        .unwrap()
                        .unwrap()
                        .0
                        .to_bool(),
                    Some(false)
                );
                app.rebuild_structure();
                assert!(!app.controlled_local_idle_for_test());
                assert_eq!(acks.get(), 1);
                assert_eq!(app.state_mut_for_test().doc.get_heads(), settled);
            }
        }
    }
}
