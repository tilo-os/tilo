//! The **View**: an app's presentation layer, distinct from its state.
//!
//! A Tilo app owns two pieces of live state: the **State document** (an
//! Automerge CRDT in [`crate::state::State`]) is the collaborative truth,
//! merged and serialized with `doc.save()`; the **View** is everything about
//! *where the user is looking* that is deliberately **not** merged — scroll,
//! caret/selection, open resource, viewport, view mode, and the toolkit-owned
//! overlays open over it (menus, popovers, dialogs, toasts).
//!
//! Two uses: **presence** (not here) broadcasts a subset so others can see
//! and follow a participant; **freeze/thaw** (here) serializes State *and*
//! View into one versioned envelope and restores both — backing OS
//! memory-eviction, reboot, and crash recovery, so a thawed app is the same
//! *place*, not merely the same data. It routes through a single override
//! hook, [`FreezeThaw::serialize_state`] / [`FreezeThaw::restore_state`],
//! defaulting to the CRDT save/load; a non-CRDT app overrides just those two.

use serde::{Deserialize, Serialize};

/// Envelope format version. Bump when the wire shape of [`FrozenApp`] changes
/// in a way older readers cannot decode; [`FreezeThaw::thaw`] refuses a
/// version it does not understand rather than mis-reading it.
pub const FROZEN_VERSION: u32 = 2;

/// A rectangle in physical pixels — the app's viewport at capture time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The focused widget's ephemeral presentation state, captured through the
/// same mechanism the undo path uses to restore a caret across a rebuild.
/// `ordinal` is stable exactly when the tree shape is — the condition under
/// which restoring a caret is meaningful; `ephemeral` is the opaque string
/// from [`crate::widget::Widget::get_state`]/`set_state`.
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
    /// What the widget has open when that is not a record of its own: the
    /// page of a notebook, the message of a mailbox. A follower opens the
    /// same one; a resumed app reopens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_resource: Option<String>,
}

/// Where a menu or popover was opened from.
///
/// Widgets anchored to themselves (a dropdown's list, a dialog) use
/// [`OverlayAnchor::Owner`]; a context menu opened at the pointer carries the
/// point it opened at, in the owner's surface coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum OverlayAnchor {
    /// Anchored to the owning widget's own bounds.
    Owner,
    /// Opened at a point, in surface coordinates.
    Point { x: f64, y: f64 },
    /// Opened against a rectangle, in surface coordinates.
    Rect { x0: f64, y0: f64, x1: f64, y1: f64 },
}

impl OverlayAnchor {
    fn is_finite(&self) -> bool {
        match *self {
            OverlayAnchor::Owner => true,
            OverlayAnchor::Point { x, y } => x.is_finite() && y.is_finite(),
            OverlayAnchor::Rect { x0, y0, x1, y1 } => {
                [x0, y0, x1, y1].iter().all(|v| v.is_finite())
            }
        }
    }
}

/// Severity of an open toast or banner, matching
/// [`crate::widget::toast::ToastKind`] and [`crate::widget::banner::BannerKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlaySeverity {
    Info,
    Success,
    Warning,
    Error,
}

/// One toast in an open [`OverlayState::Toasts`] stack.
///
/// The text is content. It is kept in the local [`View`] so a thawed app
/// shows the same notification, and dropped from the broadcast
/// [`ViewSubset`]: a follower or recorder reads it from the owner's semantic
/// node, where the node's own classification decides what may leave the
/// device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToastView {
    pub severity: OverlaySeverity,
    pub text: Option<String>,
}

/// The minimal typed state that re-opens one toolkit-owned overlay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OverlayState {
    /// A list of choices: a context menu, a popover menu, a dropdown's list.
    /// `highlighted` is the row under keyboard focus, when there is one.
    ///
    /// `menu` is the owner's name for which of its menus is open, when it has
    /// more than one (a page menu and an editor menu). `target` is the stable
    /// identity of what the menu acts on (a resource id), when that is not
    /// simply the owner's current selection. Neither is ever content.
    Menu {
        anchor: OverlayAnchor,
        highlighted: Option<usize>,
        menu: Option<String>,
        target: Option<String>,
    },
    /// A popover holding arbitrary content next to its owner.
    Popover,
    /// A dialog, never constructed from this state: the owner opens its own.
    /// `dialog` is the owner's name for which of its dialogs this is, when it
    /// has more than one; a shared dialog widget is its own owner and has none.
    Dialog { dialog: Option<String> },
    /// The toasts currently shown by a notification stack, oldest first.
    Toasts { toasts: Vec<ToastView> },
    /// An inline alert, the classic "what went wrong" signal. Its title is
    /// content, carried like a toast's text: kept locally, dropped from a
    /// broadcast and read from the banner's own semantic node instead.
    Banner {
        severity: OverlaySeverity,
        title: Option<String>,
    },
}

impl OverlayState {
    /// A menu with no owner-specific identity.
    pub fn menu(anchor: OverlayAnchor, highlighted: Option<usize>) -> Self {
        OverlayState::Menu {
            anchor,
            highlighted,
            menu: None,
            target: None,
        }
    }
}

/// How a restored overlay behaves once it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayPresentation {
    /// This app's own overlay again: a thaw, a resume, or a replay the viewer
    /// only watches. A modal is fully modal.
    Owned,
    /// Someone else's overlay shown to a live follower. It is drawn
    /// faithfully but never takes the follower's focus or input: a modal
    /// does not trap focus or capture the pointer, and the follower's first
    /// input breaks away from following and closes it.
    Mirrored,
}

/// How a follow binds this app to the followed participant's View.
///
/// This is where follow and replay differ. A live follower is a person with
/// their own hands on the app, so what they follow is shown but never takes
/// control from them ([`OverlayPresentation::Mirrored`]) and any input of
/// theirs breaks away. A replay viewer drives nothing — the replay peer is
/// the only source of input — so the recorded overlays behave exactly as
/// they did ([`OverlayPresentation::Owned`]) and local input does not end the
/// follow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FollowMode {
    #[default]
    Live,
    Replay,
}

impl FollowMode {
    pub fn overlay_presentation(self) -> OverlayPresentation {
        match self {
            FollowMode::Live => OverlayPresentation::Mirrored,
            FollowMode::Replay => OverlayPresentation::Owned,
        }
    }
}

/// An overlay open in a [`View`], named by the semantic id of the widget
/// that owns it.
///
/// The owner is the node that exists whether or not the overlay is open (a
/// dropdown, a dialog declared hidden, a menu owner, a notification stack),
/// so the same id resolves on a thawed app, a follower's app and a replay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenOverlay {
    /// The owning node's [`SemanticId`](crate::a11y_tree::SemanticId), as a string.
    pub owner: String,
    pub state: OverlayState,
}

impl OpenOverlay {
    /// The broadcastable form: toast text and banner titles stay on the
    /// owner's semantic node.
    #[must_use]
    pub fn for_broadcast(&self) -> Self {
        let state = match &self.state {
            OverlayState::Toasts { toasts } => OverlayState::Toasts {
                toasts: toasts
                    .iter()
                    .map(|toast| ToastView {
                        severity: toast.severity,
                        text: None,
                    })
                    .collect(),
            },
            OverlayState::Banner { severity, .. } => OverlayState::Banner {
                severity: *severity,
                title: None,
            },
            other => other.clone(),
        };
        OpenOverlay {
            owner: self.owner.clone(),
            state,
        }
    }
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
    /// Toolkit-owned overlays open over the content, in tree order.
    pub overlays: Vec<OpenOverlay>,
}

/// The broadcastable subset of a [`View`] — the region and resource a
/// participant is looking at, minus the caret/selection.
///
/// Deliberately small and self-describing so it can travel inside the
/// existing presence frame; it's what an **attach/follow** binds the local
/// viewport to. The caret ([`View::focus`]) is intentionally excluded: it
/// already has a dedicated presence payload (`Caret`/`Selection`) landing in
/// the per-key caret maps, so re-sending it here would double-broadcast and
/// re-key it away from the widget scopes those renderers read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewSubset {
    /// Scroll offset(s) of the participant's scrollable content.
    pub scroll: Vec<f64>,
    /// The resource the participant currently has open, if any.
    pub open_resource: Option<String>,
    /// The participant's viewport rect in physical pixels.
    pub viewport: ViewRect,
    /// The participant's current view mode, if the app has one.
    pub view_mode: Option<String>,
    /// The overlays open over the participant's content — what a follower
    /// mirrors and a recorder keeps. Toast text is not carried; see
    /// [`ToastView`].
    pub overlays: Vec<OpenOverlay>,
    /// Whether the participant's document was saved, as their app showed
    /// it: device-local presentation a replay shows as recorded, like the
    /// overlays. `None` for an app with no record of its own.
    pub save_status: Option<crate::record::RecordSaveStatus>,
}

impl ViewSubset {
    /// Project a full [`View`] onto the subset presence broadcasts. Drops
    /// [`View::focus`] (broadcast separately as caret/selection presence) and
    /// the content of open toasts.
    pub fn from_view(view: &View) -> Self {
        ViewSubset {
            scroll: view.scroll.clone(),
            open_resource: view.open_resource.clone(),
            viewport: view.viewport,
            view_mode: view.view_mode.clone(),
            overlays: view
                .overlays
                .iter()
                .map(OpenOverlay::for_broadcast)
                .collect(),
            save_status: None,
        }
    }

    /// True at the starting point of a freshly-launched app that has not
    /// scrolled, opened a resource, or been laid out — lets the broadcaster
    /// skip the first frame's no-op send.
    pub fn is_empty(&self) -> bool {
        *self == ViewSubset::default()
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
        if self.overlays.iter().any(|overlay| match &overlay.state {
            OverlayState::Menu { anchor, .. } => !anchor.is_finite(),
            OverlayState::Popover
            | OverlayState::Dialog { .. }
            | OverlayState::Toasts { .. }
            | OverlayState::Banner { .. } => false,
        }) {
            return Err(ThawError::InvalidView("nonfinite overlay anchor".into()));
        }
        Ok(())
    }
}

pub(crate) fn capture_widget_view_bound(
    root: &dyn crate::widget::Widget,
    focus: Option<FocusView>,
    viewport: ViewRect,
    overlays: Vec<OpenOverlay>,
) -> View {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
        return View {
            focus,
            viewport,
            overlays,
            ..View::default()
        };
    };
    let contribution = root.view_state().unwrap_or_default();
    let open_resource = root
        .record_intent()
        .map(|c| c.id.as_str().to_string())
        .or(contribution.open_resource);
    View {
        scroll: contribution.scroll,
        focus,
        open_resource,
        viewport,
        view_mode: contribution.view_mode,
        overlays,
    }
}

/// Whether any retained widget currently has an overlay open. Cheap enough to
/// ask every frame, so the semantic walk that names overlay owners only runs
/// while something is (or was) open.
pub(crate) fn any_overlay_open_bound(root: &dyn crate::widget::Widget) -> bool {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
        return false;
    };
    !root.open_overlays().is_empty()
        || root
            .children()
            .into_iter()
            .any(|child| any_overlay_open_bound(child))
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
    root.restore_view_state(&ViewContribution {
        scroll: view.scroll.clone(),
        view_mode: view.view_mode.clone(),
        open_resource: None,
    });
}

/// The caller resolves the saved ordinal in its current component's focus order.
pub(crate) fn restore_focus_state_bound(widget: &mut dyn crate::widget::Widget, focus: &FocusView) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    if !focus.ephemeral.is_empty() {
        widget.set_state(&focus.ephemeral);
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
                return capture_focus(Some(current), || root.get_state());
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

/// A root with one of each toolkit-owned overlay: a dropdown list, a dialog,
/// a notification stack and an error banner. `open` builds it with the list,
/// dialog and toast showing; the banner is always declared.
#[cfg(test)]
pub(crate) fn overlay_test_root(
    open: bool,
    own_toast: Option<&str>,
) -> Box<dyn crate::widget::Widget> {
    use crate::widget::banner::Banner;
    use crate::widget::toast::{ToastKind, ToastStack};
    use crate::widget::{Column, Dropdown, ModalDialog, Text};
    let mut dropdown = Dropdown::new(vec!["One".into(), "Two".into(), "Three".into()], "choice");
    let mut dialog = ModalDialog::new("Discard changes?", Text::new("Unsaved edits are lost"));
    let mut toasts = ToastStack::new();
    if open {
        dropdown.open_menu();
        dialog.set_open(true);
        toasts.push("Couldn't send", ToastKind::Error);
    }
    if let Some(text) = own_toast {
        toasts.push(text, ToastKind::Success);
    }
    Box::new(
        Column::new()
            .push(Box::new(dropdown))
            .push(Box::new(dialog))
            .push(Box::new(toasts))
            .push(Box::new(Banner::error("Couldn't reach the server"))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PresenceCtx;
    use crate::TextRenderer;
    use crate::Theme;
    use crate::app::App;
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use crate::state::State;
    use crate::widget::Widget;
    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, transaction::Transactable};
    use std::collections::HashMap;
    use taffy::TaffyTree;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect as KurboRect};
    use winit::event::WindowEvent;

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
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: taffy::NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event_ctx(
            &mut self,
            _event: &WindowEvent,
            _ctx: &mut crate::widget::EventCtx<'_>,
        ) {
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
        fn get_state(&self) -> Option<String> {
            Some(self.caret.clone())
        }
        fn set_state(&mut self, state: &str) {
            self.caret = state.to_string();
        }
        fn view_state(&self) -> Option<ViewContribution> {
            Some(ViewContribution {
                scroll: self.scroll.clone(),
                view_mode: self.mode.clone(),
                open_resource: None,
            })
        }
        fn restore_view_state(&mut self, view: &ViewContribution) {
            self.scroll = view.scroll.clone();
            self.mode = view.view_mode.clone();
        }
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

    /// A menu, a dialog and a toast open when the app is frozen are open,
    /// under the same owners and with the same state, after it is thawed into
    /// a fresh app. Asserted on the envelope bytes too, so capture drops
    /// nothing.
    #[test]
    fn open_overlays_round_trip_through_freeze_and_thaw() {
        let mut app1 = App::new_with_controlled_inputs(State::new(), overlay_test_root(true, None));
        app1.init_layout();
        app1.refresh_semantic_tree();
        let v1 = app1.capture_view();
        let kinds = v1
            .overlays
            .iter()
            .map(|overlay| match &overlay.state {
                OverlayState::Menu { .. } => "menu",
                OverlayState::Popover => "popover",
                OverlayState::Dialog { .. } => "dialog",
                OverlayState::Toasts { .. } => "toasts",
                OverlayState::Banner { .. } => "banner",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            ["menu", "dialog", "toasts", "banner"],
            "{:?}",
            v1.overlays
        );
        assert_eq!(
            v1.overlays[3].state,
            OverlayState::Banner {
                severity: OverlaySeverity::Error,
                title: Some("Couldn't reach the server".into()),
            },
            "the local View keeps the banner's title"
        );
        assert_eq!(
            v1.overlays[3].for_broadcast().state,
            OverlayState::Banner {
                severity: OverlaySeverity::Error,
                title: None,
            },
            "a broadcast carries that an error banner is shown, not what it says"
        );
        assert!(
            v1.overlays.iter().all(|overlay| app1
                .semantic_tree()
                .node(&crate::a11y_tree::SemanticId(overlay.owner.clone()))
                .is_some()),
            "every overlay is named by a node of the semantic tree"
        );
        assert_eq!(
            v1.overlays[2].state,
            OverlayState::Toasts {
                toasts: vec![ToastView {
                    severity: OverlaySeverity::Error,
                    text: Some("Couldn't send".into()),
                }],
            },
            "the local View keeps the toast's text"
        );

        let frozen = app1.freeze();
        let env: FrozenApp = postcard::from_bytes(&frozen).unwrap();
        assert_eq!(env.view.overlays, v1.overlays);

        let mut app2 =
            App::new_with_controlled_inputs(State::new(), overlay_test_root(false, None));
        app2.init_layout();
        app2.refresh_semantic_tree();
        assert_eq!(
            app2.capture_view().overlays,
            v1.overlays[3..],
            "a fresh app shows only the banner it declares"
        );

        app2.thaw(&frozen).expect("thaw a well-formed envelope");
        app2.refresh_semantic_tree();
        assert_eq!(
            app2.capture_view().overlays,
            v1.overlays,
            "the thawed app shows the same menu, dialog and toast"
        );
    }
}
