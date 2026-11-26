//! The **View**: an app's presentation layer, distinct from its state. A Tilo app owns
//! two pieces of live state: the **State document** (an Automerge CRDT in
//! [`crate::state::State`]) is the collaborative truth, merged and serialized with
//! `doc.save()`; the **View** is everything about *where the user is looking* that is
//! deliberately **not** merged — scroll, caret/selection, open resource, viewport, view
//! mode.

pub mod capability;
use serde::{Deserialize, Serialize};

/// Envelope format version. Bump when the wire shape of [`FrozenApp`] changes in a way
/// older readers cannot decode; [`FreezeThaw::thaw`] refuses a version it does not
/// understand rather than mis-reading it.
pub const FROZEN_VERSION: u32 = 1;

pub use tilo_presence::{ViewRect, ViewSubset};

/// The focused widget's ephemeral presentation state, captured through the same mechanism
/// the undo path uses to restore a caret across a rebuild.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FocusView {
    pub ordinal: usize,
    pub ephemeral: String,
}

/// The typed presentation fields a widget contributes to (and has restored from) the
/// [`View`] — its scroll offset(s) and its current view mode.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewContribution {
    pub scroll: Vec<f64>,
    pub view_mode: Option<String>,
}

/// A member's View: everything about *where they are looking* that is never merged
/// into anyone else's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub scroll: Vec<f64>,
    pub focus: Option<FocusView>,
    pub open_resource: Option<String>,
    /// The viewport rect in physical pixels at capture time.
    pub viewport: ViewRect,
    pub view_mode: Option<String>,
}

/// Project a full [`View`] onto the subset presence broadcasts.
pub fn broadcast_subset(view: &View) -> ViewSubset {
    ViewSubset {
        scroll: view.scroll.clone(),
        open_resource: view.open_resource.clone(),
        viewport: view.viewport,
        view_mode: view.view_mode.clone(),
    }
}

/// Common local presentation validation; canonical state is never read or written here.
impl View {
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

/// What `root` shows, as a [`View`]: its scroll and view mode, and the record it has open. Only
/// Tilo's model opens a record, so without it a view names none.
pub(crate) fn capture_widget_view_bound(
    root: &dyn tilo_ui_base::widget::Widget,
    focus: Option<FocusView>,
    viewport: ViewRect,
) -> View {
    #[cfg(feature = "model")]
    let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(root) else {
        return View {
            focus,
            viewport,
            ..View::default()
        };
    };
    #[cfg(feature = "model")]
    let open_resource =
        crate::resource::capability::record_intent(root).map(|c| c.id.as_str().to_string());
    #[cfg(not(feature = "model"))]
    let open_resource = None;
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

/// Restore `view`'s scroll and view mode onto `root`.
pub(crate) fn restore_widget_view_bound(root: &mut dyn tilo_ui_base::widget::Widget, view: &View) {
    #[cfg(feature = "model")]
    let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(root) else {
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
pub(crate) fn restore_focus_state_bound(
    widget: &mut dyn tilo_ui_base::widget::Widget,
    focus: &FocusView,
) {
    let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(widget) else {
        return;
    };
    if !focus.ephemeral.is_empty() {
        tilo_ui_base::widget::undo_points::set_state(widget, &focus.ephemeral);
    }
}

/// Prepared components have no runner node map yet.
pub(crate) fn capture_tree_focus_bound(
    root: &dyn tilo_ui_base::widget::Widget,
) -> Option<FocusView> {
    fn visit(root: &dyn tilo_ui_base::widget::Widget, ordinal: &mut usize) -> Option<FocusView> {
        let _entry = tilo_ui_base::widget::bound_nodes::enter_widget(root).ok()?;
        if root.is_focusable() && root.is_enabled() {
            let current = *ordinal;
            *ordinal = ordinal.saturating_add(1);
            if root.is_focused() {
                return capture_focus(Some(current), || {
                    tilo_ui_base::widget::undo_points::get_state(root)
                });
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

pub(crate) fn restore_tree_focus_bound(
    root: &mut dyn tilo_ui_base::widget::Widget,
    focus: &FocusView,
) {
    fn visit(root: &mut dyn tilo_ui_base::widget::Widget, ordinal: &mut usize, focus: &FocusView) {
        let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(root) else {
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
    fn count(root: &dyn tilo_ui_base::widget::Widget) -> usize {
        let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(root) else {
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

/// The versioned freeze envelope: an app's serialized state bytes alongside its captured
/// [`View`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenApp {
    pub version: u32,
    pub state: Vec<u8>,
    pub view: View,
}

/// Why a [`FreezeThaw::thaw`] failed. Thaw never panics — truncated or garbage bytes, an
/// unknown version, or an unloadable state payload all surface here.
#[derive(Debug)]
pub enum ThawError {
    /// The envelope itself could not be decoded (truncated / garbage bytes).
    Envelope(postcard::Error),
    UnsupportedVersion {
        found: u32,
        supported: u32,
    },
    /// The state payload failed to load — a corrupt CRDT document, or an override impl's
    /// own decode error.
    State(String),
    InvalidView(String),
    BindingMismatch,
    Io(std::io::Error),
    /// The workspace, which keeps every saved view, did not answer the read: its service on this
    /// device is down or slow.
    Unreachable(String),
    /// Local View storage is still opening the View on a thread that does not wait for
    /// the disk; it wakes that thread once it is ready.
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
            ThawError::Unreachable(reason) => write!(f, "the workspace did not answer: {reason}"),
            ThawError::Opening => f.write_str("local view storage is still opening"),
            ThawError::State(reason) => write!(f, "frozen-app state failed to load: {reason}"),
        }
    }
}

impl std::error::Error for ThawError {}

/// Freeze/thaw for a Tilo app, extendable at exactly one boundary.
pub trait FreezeThaw {
    /// Serialize this app's state to bytes.
    fn serialize_state(&self) -> Vec<u8>;

    /// Load state bytes produced by [`serialize_state`](Self::serialize_state). Returns
    /// an error if `bytes` is not a payload this implementor previously produced via
    /// `serialize_state` — truncated, corrupt, or from an incompatible version.
    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), ThawError>;

    /// Capture this app's presentation into a [`View`].
    fn capture_view(&self) -> View {
        View::default()
    }

    /// Restore a [`View`] onto this app.
    fn restore_view(&mut self, _view: &View) {}

    /// Serialize state **and** view into one versioned envelope.
    fn freeze(&self) -> Vec<u8> {
        let env = FrozenApp {
            version: FROZEN_VERSION,
            state: self.serialize_state(),
            view: self.capture_view(),
        };
        // These are all owned, self-describing types; postcard serialization of them is
        // infallible, so a serialization error here can only mean an internal postcard
        // bug — fall back to an empty payload rather than panic.
        postcard::to_stdvec(&env).unwrap_or_default()
    }

    /// Restore state and view from bytes produced by [`freeze`](Self::freeze). Returns an
    /// error — never panics — on truncated/garbage bytes, an unknown version, or an
    /// unloadable state payload.
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
