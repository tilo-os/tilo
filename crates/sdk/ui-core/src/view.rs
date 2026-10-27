//! The **View**: an app's presentation layer, distinct from its state.
//!
//! A Tilo app owns two pieces of live state: the **State document** (an
//! Automerge CRDT in [`crate::state::State`]) is the collaborative truth,
//! merged and serialized with `doc.save()`; the **View** is everything about
//! *where the user is looking* that is deliberately **not** merged — scroll,
//! caret/selection, open resource, viewport, view mode.
//!
//! Two uses: **presence** (not here) broadcasts a subset so others can see
//! and follow a participant; **freeze/thaw** (here) serializes State *and*
//! View into one versioned envelope and restores both — backing OS
//! memory-eviction, reboot, and crash recovery, so a thawed app is the same
//! *place*, not merely the same data. It routes through a single override
//! hook, [`FreezeThaw::serialize_state`] / [`FreezeThaw::restore_state`],
//! defaulting to the CRDT save/load; a non-CRDT app overrides just those two.

pub mod capability;
use serde::{Deserialize, Serialize};

/// Envelope format version. Bump when the wire shape of [`FrozenApp`] changes
/// in a way older readers cannot decode; [`FreezeThaw::thaw`] refuses a
/// version it does not understand rather than mis-reading it.
pub const FROZEN_VERSION: u32 = 1;

pub use collab_types::presence::{ViewRect, ViewSubset};

/// The focused widget's ephemeral presentation state, captured through the
/// same mechanism the undo path uses to restore a caret across a rebuild.
/// `ordinal` is stable exactly when the tree shape is — the condition under
/// which restoring a caret is meaningful; `ephemeral` is the opaque string
/// from [`crate::undo::UndoPoints::get_state`]/`set_state`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FocusView {
    pub ordinal: usize,
    pub ephemeral: String,
}

/// The typed presentation fields a widget contributes to (and has restored
/// from) the [`View`] — its scroll offset(s) and its current view mode.
/// Kept structured rather than folded into [`FocusView::ephemeral`] because a
/// presence layer wants to broadcast scroll/view-mode on their own — "this
/// peer is scrolled here, in grid mode" — independent of whether the
/// scrolling container is the focused widget.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewContribution {
    /// Scroll offset(s). A single scrollable owns one value; a multi-pane
    /// layout can carry one per pane. Empty when nothing scrolls.
    pub scroll: Vec<f64>,
    /// The app's current view mode (e.g. `"grid"` / `"list"`), if it has one.
    pub view_mode: Option<String>,
}

/// A participant's View: everything about *where they are looking* that is
/// never merged into anyone else's. Every field derives
/// `Serialize`/`Deserialize` so a future presence layer can broadcast any
/// subset. [`View::default`] is "top of the document, nothing focused, no
/// resource" — the correct starting point for a freshly-launched app.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// Scroll offset(s) of the app's scrollable content.
    pub scroll: Vec<f64>,
    /// The focused widget's caret/selection, or `None` when nothing is focused.
    pub focus: Option<FocusView>,
    /// The id of the resource currently open (a record id), or `None`.
    pub open_resource: Option<String>,
    /// The viewport rect in physical pixels at capture time.
    pub viewport: ViewRect,
    /// The current view mode, if the app has one.
    pub view_mode: Option<String>,
}

/// Project a full [`View`] onto the subset presence broadcasts. Drops
/// [`View::focus`] (broadcast separately as caret/selection presence).
pub fn broadcast_subset(view: &View) -> ViewSubset {
    ViewSubset {
        scroll: view.scroll.clone(),
        open_resource: view.open_resource.clone(),
        viewport: view.viewport,
        view_mode: view.view_mode.clone(),
    }
}

impl View {
    /// Capture the live [`App`](crate::app::App)'s presentation into a View.
    ///
    /// Thin front door for [`FreezeThaw::capture_view`] so callers can write
    /// the natural `View::capture(&app)`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn capture(app: &crate::app::App) -> View {
        <crate::app::App as FreezeThaw>::capture_view(app)
    }

    /// Write this View back onto a live [`App`](crate::app::App): move focus
    /// and restore the caret, hand the scroll/view-mode fields to the root
    /// widget. The counterpart to [`View::capture`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore(&self, app: &mut crate::app::App) {
        <crate::app::App as FreezeThaw>::restore_view(app, self);
    }
}

/// Common local presentation validation; canonical state is never read or written here.
impl View {
    /// # Errors
    /// Rejects nonfinite geometry before any widget receives it.
    pub fn validate(&self) -> Result<(), ThawError> {
        let rect = self.viewport;
        if self
            .scroll
            .iter()
            .chain([&rect.x, &rect.y, &rect.width, &rect.height])
            .any(|v| !v.is_finite())
        {
            return Err(ThawError::InvalidView("nonfinite geometry".into()));
        }
        Ok(())
    }
}

pub(crate) fn capture_widget_view_bound(
    root: &dyn crate::widget::Widget,
    focus: Option<FocusView>,
    viewport: ViewRect,
) -> View {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
        return View {
            focus,
            viewport,
            ..View::default()
        };
    };
    let open_resource =
        crate::record::capability::record_intent(root).map(|c| c.id.as_str().to_string());
    let contribution = crate::view::capability::view_state(root).unwrap_or_default();
    View {
        scroll: contribution.scroll,
        focus,
        open_resource,
        viewport,
        view_mode: contribution.view_mode,
    }
}

pub(crate) fn capture_focus(
    ordinal: Option<usize>,
    state: impl FnOnce() -> Option<String>,
) -> Option<FocusView> {
    ordinal.map(|ordinal| FocusView {
        ordinal,
        ephemeral: state().unwrap_or_default(),
    })
}

pub(crate) fn restore_widget_view_bound(root: &mut dyn crate::widget::Widget, view: &View) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
        return;
    };
    crate::view::capability::restore_view_state(
        root,
        &ViewContribution {
            scroll: view.scroll.clone(),
            view_mode: view.view_mode.clone(),
        },
    );
}

/// The caller resolves the saved ordinal in its current component's focus order.
pub(crate) fn restore_focus_state_bound(widget: &mut dyn crate::widget::Widget, focus: &FocusView) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    if !focus.ephemeral.is_empty() {
        crate::undo::set_state(widget, &focus.ephemeral);
    }
}

/// Prepared components have no runner node map yet. Traverse only their retained tree.
pub(crate) fn capture_tree_focus_bound(root: &dyn crate::widget::Widget) -> Option<FocusView> {
    fn visit(root: &dyn crate::widget::Widget, ordinal: &mut usize) -> Option<FocusView> {
        let _entry = crate::bound_dispatch::enter_widget(root).ok()?;
        if root.is_focusable() && root.is_enabled() {
            let current = *ordinal;
            *ordinal = ordinal.saturating_add(1);
            if root.is_focused() {
                return capture_focus(Some(current), || crate::undo::get_state(root));
            }
        }
        for child in root.children() {
            if let Some(focus) = visit(child, ordinal) {
                return Some(focus);
            }
        }
        None
    }
    visit(root, &mut 0)
}

pub(crate) fn restore_tree_focus_bound(root: &mut dyn crate::widget::Widget, focus: &FocusView) {
    fn visit(root: &mut dyn crate::widget::Widget, ordinal: &mut usize, focus: &FocusView) {
        let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
            return;
        };
        if root.is_focusable() && root.is_enabled() {
            let selected = *ordinal == focus.ordinal;
            *ordinal = ordinal.saturating_add(1);
            root.set_focus(selected);
            if selected {
                restore_focus_state_bound(root, focus);
            }
        }
        for child in root.children_mut() {
            visit(child, ordinal, focus);
        }
    }
    // An ordinal absent from the current tree must leave existing focus alone.
    fn count(root: &dyn crate::widget::Widget) -> usize {
        let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
            return 0;
        };
        root.children().iter().fold(
            usize::from(root.is_focusable() && root.is_enabled()),
            |total, child| total.saturating_add(count(*child)),
        )
    }
    if focus.ordinal < count(root) {
        visit(root, &mut 0, focus);
    }
}

/// The versioned freeze envelope: an app's serialized state bytes alongside
/// its captured [`View`]. Collaboration and freeze/thaw serialize the *same*
/// state; this pairs it with the presentation so a restore lands the user back
/// where they were, not merely on the same data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenApp {
    pub version: u32,
    pub state: Vec<u8>,
    pub view: View,
}

/// Why a [`FreezeThaw::thaw`] failed. Thaw never panics — truncated or garbage
/// bytes, an unknown version, or an unloadable state payload all surface here.
#[derive(Debug)]
pub enum ThawError {
    /// The envelope itself could not be decoded (truncated / garbage bytes).
    Envelope(postcard::Error),
    /// The envelope declared a version this build does not understand.
    UnsupportedVersion { found: u32, supported: u32 },
    /// The state payload failed to load — a corrupt CRDT document, or an
    /// override impl's own decode error. Carries a human-readable reason.
    State(String),
    /// Invalid local presentation or identity.
    InvalidView(String),
    /// A local envelope belongs to a different host binding.
    BindingMismatch,
    /// Local View storage failed.
    Io(std::io::Error),
    /// Local View storage is still opening the View on a thread that does
    /// not wait for the disk; it wakes that thread once it is ready.
    Opening,
}

impl std::fmt::Display for ThawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThawError::Envelope(e) => write!(f, "frozen-app envelope decode failed: {e}"),
            ThawError::UnsupportedVersion { found, supported } => write!(
                f,
                "frozen-app version {found} is not supported (this build reads {supported})"
            ),
            ThawError::InvalidView(reason) => write!(f, "invalid local view: {reason}"),
            ThawError::BindingMismatch => f.write_str("local view binding does not match"),
            ThawError::Io(error) => write!(f, "local view storage failed: {error}"),
            ThawError::Opening => f.write_str("local view storage is still opening"),
            ThawError::State(reason) => write!(f, "frozen-app state failed to load: {reason}"),
        }
    }
}

impl std::error::Error for ThawError {}

/// Freeze/thaw for a Tilo app, extendable at exactly one seam.
///
/// The two **required** methods are the override hook: `serialize_state` /
/// `restore_state` say how an app's *state* becomes bytes and back,
/// defaulting (for a [`crate::app::App`]) to the CRDT `doc.save()` /
/// `AutoCommit::load`; a non-CRDT app overrides only these.
///
/// `capture_view` / `restore_view` default to an empty View / a no-op, so an
/// implementor that doesn't touch the toolkit's focus/scroll machinery still
/// round-trips its state; [`crate::app::App`] overrides them for the real
/// focus/scroll/resource/viewport.
///
/// `freeze` / `thaw` are **not** meant to be overridden: they frame
/// `serialize_state() + capture_view()` into a versioned [`FrozenApp`] and
/// back, so overriding the state hook changes only serialization.
pub trait FreezeThaw {
    /// Serialize this app's state to bytes. The single override point for a
    /// non-CRDT app.
    fn serialize_state(&self) -> Vec<u8>;

    /// Load state bytes produced by [`serialize_state`](Self::serialize_state).
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a payload this implementor
    /// previously produced via `serialize_state` — truncated, corrupt, or
    /// from an incompatible version.
    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), ThawError>;

    /// Capture this app's presentation into a [`View`]. Default: an empty View,
    /// so an implementor with no toolkit-managed presentation still round-trips.
    fn capture_view(&self) -> View {
        View::default()
    }

    /// Restore a [`View`] onto this app. Default: no-op.
    fn restore_view(&mut self, _view: &View) {}

    /// Serialize state **and** view into one versioned envelope. Not an
    /// override point — overriding [`serialize_state`](Self::serialize_state)
    /// is enough to change what this produces.
    fn freeze(&self) -> Vec<u8> {
        let env = FrozenApp {
            version: FROZEN_VERSION,
            state: self.serialize_state(),
            view: self.capture_view(),
        };
        // These are all owned, self-describing types; postcard serialization of
        // them is infallible, so a serialization error here can only mean an
        // internal postcard bug — fall back to an empty payload rather than
        // panic.
        postcard::to_stdvec(&env).unwrap_or_default()
    }

    /// Restore state and view from bytes produced by [`freeze`](Self::freeze).
    ///
    /// # Errors
    /// Returns an error — never panics — on truncated/garbage bytes, an unknown
    /// version, or an unloadable state payload.
    fn thaw(&mut self, bytes: &[u8]) -> Result<(), ThawError> {
        let env: FrozenApp = postcard::from_bytes(bytes).map_err(ThawError::Envelope)?;
        if env.version != FROZEN_VERSION {
            return Err(ThawError::UnsupportedVersion {
                found: env.version,
                supported: FROZEN_VERSION,
            });
        }
        self.restore_state(&env.state)?;
        self.restore_view(&env.view);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;
    use crate::app::App;
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use crate::state::State;
    use crate::widget::TaffyTreeExt;
    use crate::widget::Widget;
    use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
    use std::collections::HashMap;
    use taffy::TaffyTree;
    use vello::kurbo::Rect as KurboRect;

    /// A minimal root widget that owns all the presentation state a View
    /// captures: a caret (via `get_state`/`set_state`), a scroll offset and a
    /// view mode (via `view_state`/`restore_view_state`), and an open resource
    /// (via `record_intent`). Focusable, so it lands in `focusable_order`.
    struct DocRoot {
        caret: String,
        scroll: Vec<f64>,
        mode: Option<String>,
        resource: String,
        focused: bool,
        rect: KurboRect,
    }

    impl DocRoot {
        fn new() -> Self {
            Self {
                caret: String::new(),
                scroll: Vec::new(),
                mode: None,
                resource: "file:/notes.md".into(),
                focused: false,
                rect: KurboRect::ZERO,
            }
        }
    }

    impl Widget for DocRoot {
        crate::provides!(
            dyn crate::record::capability::RecordScoped,
            dyn crate::view::capability::ViewState,
            dyn crate::undo::UndoPoints
        );

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            taffy.widget_leaf(taffy::Style::default(), node_map, self)
        }
        fn name(&self) -> &'static str {
            "doc-root"
        }
        fn get_rect(&self) -> KurboRect {
            self.rect
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn set_focus(&mut self, focused: bool) {
            self.focused = focused;
        }
        fn is_focused(&self) -> bool {
            self.focused
        }
    }

    impl crate::undo::UndoPoints for DocRoot {
        fn get_state(&self) -> Option<String> {
            Some(self.caret.clone())
        }
        fn set_state(&mut self, state: &str) {
            self.caret = state.to_string();
        }
    }

    impl crate::view::capability::ViewState for DocRoot {
        fn view_state(&self) -> Option<ViewContribution> {
            Some(ViewContribution {
                scroll: self.scroll.clone(),
                view_mode: self.mode.clone(),
            })
        }
        fn restore_view_state(&mut self, view: &ViewContribution) {
            self.scroll = view.scroll.clone();
            self.mode = view.view_mode.clone();
        }
    }

    impl crate::record::capability::RecordScoped for DocRoot {
        fn record_intent(&self) -> Option<RecordCheckout> {
            RecordId::parse(&self.resource)
                .map(|id| RecordCheckout::new(id, RecordKind::new("document")))
        }
    }

    fn text_at(doc: &AutoCommit, key: &str) -> Option<String> {
        let (_, obj) = doc.get(&ROOT, key).ok()??;
        doc.text(&obj).ok()
    }

    #[test]
    fn freeze_and_thaw_round_trip_doc_and_view_and_reject_bad_bytes() {
        // Case `freeze_thaw_round_trips_doc_and_view`:
        // A populated doc + a non-default View (scrolled, a caret, an open
        // resource, a view mode) freezes and thaws into a *fresh* app with BOTH
        // the document content AND every view field intact — asserted on the
        // real freeze/thaw path, and on the bytes in between so no field is
        // silently dropped by capture.
        {
            #[cfg(windows)]
            let file_record = r"file:C:\report.md";
            #[cfg(not(windows))]
            let file_record = "file:/report.md";
            // App 1: a document with real text, and a root carrying a non-default
            // presentation (scroll / caret / mode / open resource).
            let mut root = DocRoot::new();
            root.caret = "caret=42".into();
            root.scroll = vec![10.0, 20.0];
            root.mode = Some("grid".into());
            root.resource = file_record.into();

            let mut state = State::new();
            let body = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&body, 0, 0, "Hello, world").unwrap();
            // Stamp the document with the record identity it holds, so reopening
            // it after thaw is readable from the restored document.
            crate::record::init(
                &mut state.doc,
                &RecordId::parse(file_record).unwrap(),
                &RecordKind::new("document"),
                "home-room",
            );

            let mut app1 = App::new_with_controlled_inputs(state, Box::new(root));
            app1.autofocus_first = true; // focus the single field so a caret exists
            app1.init_layout();

            let v1 = app1.capture_view();
            assert_eq!(v1.scroll, vec![10.0, 20.0]);
            assert_eq!(v1.view_mode.as_deref(), Some("grid"));
            assert_eq!(v1.open_resource.as_deref(), Some(file_record));
            assert_eq!(v1.focus.as_ref().unwrap().ordinal, 0);
            assert_eq!(v1.focus.as_ref().unwrap().ephemeral, "caret=42");

            let frozen = app1.freeze();

            // The bytes in between carry every field — capture didn't drop any.
            let env: FrozenApp = postcard::from_bytes(&frozen).unwrap();
            assert_eq!(env.version, FROZEN_VERSION);
            assert_eq!(env.view, v1);

            // App 2: a fresh app with an EMPTY doc and a default root.
            let mut app2 = App::new_with_controlled_inputs(State::new(), Box::new(DocRoot::new()));
            app2.init_layout();
            assert_eq!(
                text_at(&app2.state_mut_for_test().doc.clone(), "body"),
                None
            );

            app2.thaw(&frozen).expect("thaw a well-formed envelope");

            let doc2 = app2.state_mut_for_test().doc.clone();
            assert_eq!(
                text_at(&doc2, "body").as_deref(),
                Some("Hello, world"),
                "the CRDT document round-tripped through freeze/thaw"
            );
            // The restored document carries the record identity that
            // `open_resource` named — the resource is reopened.
            assert_eq!(
                crate::record::identity(&doc2).map(|c| c.id.as_str().to_string()),
                Some(file_record.to_string()),
                "thaw reopened the resource the View named — its document is live"
            );

            // `scroll`/`view_mode` went back to the root widget; `focus`/caret via
            // the ephemeral get_state/set_state mechanism. `open_resource` isn't
            // re-checked here — it's proven above via the live document, since the
            // document *is* the resource.
            let v2 = app2.capture_view();
            assert_eq!(v2.scroll, vec![10.0, 20.0], "scroll restored to the widget");
            assert_eq!(v2.view_mode.as_deref(), Some("grid"), "view mode restored");
            let f2 = v2.focus.as_ref().expect("focus restored");
            assert_eq!(f2.ordinal, 0, "focus ordinal restored");
            assert_eq!(f2.ephemeral, "caret=42", "caret restored via set_state");
        }

        // Case `thaw_rejects_bad_bytes_without_panicking`:
        // Thawing truncated or garbage bytes returns an error, and an envelope
        // from an unknown version is rejected rather than mis-read — never a panic.
        {
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(DocRoot::new()));
            app.init_layout();

            assert!(matches!(
                app.thaw(&[0xff, 0x00, 0x7e, 0x13]),
                Err(ThawError::Envelope(_))
            ));
            assert!(matches!(app.thaw(&[]), Err(ThawError::Envelope(_))));

            // A valid envelope truncated mid-way still fails cleanly.
            let good = app.freeze();
            let truncated = &good[..good.len() / 2];
            assert!(
                app.thaw(truncated).is_err(),
                "truncated bytes error, not panic"
            );

            // A future version is refused, not silently mis-decoded.
            let future = FrozenApp {
                version: FROZEN_VERSION + 99,
                state: AutoCommit::new().save(),
                view: View::default(),
            };
            let bytes = postcard::to_stdvec(&future).unwrap();
            assert!(matches!(
                app.thaw(&bytes),
                Err(ThawError::UnsupportedVersion { .. })
            ));
        }

        // Case `custom_serialization_app_round_trips_via_override`:
        // A stub whose state is NOT a CRDT overrides only `serialize_state` /
        // `restore_state` and round-trips through the *same* `freeze`/`thaw`
        // default path — proving that view capture/restore default cleanly for it.
        {
            /// A browser-like app: its "state" is an opaque session string, not a
            /// CRDT. It carries a View too, to prove the defaults compose.
            struct BrowserStub {
                session: String,
            }

            impl FreezeThaw for BrowserStub {
                fn serialize_state(&self) -> Vec<u8> {
                    self.session.clone().into_bytes()
                }
                fn restore_state(&mut self, bytes: &[u8]) -> Result<(), ThawError> {
                    self.session = String::from_utf8(bytes.to_vec())
                        .map_err(|e| ThawError::State(e.to_string()))?;
                    Ok(())
                }
                // capture_view / restore_view intentionally left as the defaults.
            }

            let src = BrowserStub {
                session: "tabs=[example.com, tilo.so]; active=1".into(),
            };
            let frozen = src.freeze();

            let mut dst = BrowserStub {
                session: String::new(),
            };
            dst.thaw(&frozen).expect("thaw the browser stub");
            assert_eq!(
                dst.session, "tabs=[example.com, tilo.so]; active=1",
                "the non-CRDT state round-tripped through the serialize_state hook"
            );

            // The default view capture is an empty View, and it survives the frame.
            let env: FrozenApp = postcard::from_bytes(&frozen).unwrap();
            assert_eq!(env.view, View::default());

            // A malformed state payload surfaces as a State error, not a panic.
            let bad = FrozenApp {
                version: FROZEN_VERSION,
                state: vec![0xff, 0xfe, 0xfd],
                view: View::default(),
            };
            let bad_bytes = postcard::to_stdvec(&bad).unwrap();
            assert!(matches!(dst.thaw(&bad_bytes), Err(ThawError::State(_))));
        }
    }
}
