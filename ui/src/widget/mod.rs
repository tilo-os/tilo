use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::permissions::PermissionScope;
use crate::dnd::DragPayload;

use std::collections::HashMap;

/// Group opacity applied to a widget's painted output when it is
/// disabled (`Widget::is_enabled() == false`). Kept in one place so the
/// dimmed look is consistent across Button / Checkbox / Toggle / Slider /
/// Dropdown — wrap the widget's draw output in a
/// `scene.push_layer(.., DISABLED_ALPHA, ..)` / `pop_layer()` pair.
pub const DISABLED_ALPHA: f32 = 0.45;

pub trait Widget {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId);
    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool { false }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId;
    fn measure(&mut self, _known: taffy::Size<Option<f32>>, _available: taffy::Size<taffy::AvailableSpace>, _text_renderer: &mut TextRenderer) -> taffy::Size<f32> { taffy::Size::ZERO }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point);
    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx);
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>);
    fn children(&self) -> Vec<&dyn Widget> { vec![] }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> { vec![] }
    fn name(&self) -> &str;
    /// Optional stable identifier used by the e2e test harness to address
    /// this widget by name. Returns `Some(id)` only for hand-picked, high-
    /// value targets (palette input, wheel disc, key buttons, command
    /// palette items). The driver's `widget_registry()` falls back to a
    /// path-based key (`Parent/Child[index]`) when this is `None`, so
    /// most widgets need not override it.
    ///
    /// `test_id` values must be unique across the live tree at any given
    /// frame — the registry resolves on first match and a duplicate ID
    /// would shadow the second occurrence. Convention: snake_case names
    /// describing the role, not the widget type (e.g. `command_palette_input`,
    /// not `TextInput`).
    fn test_id(&self) -> Option<&str> { None }

    /// Named hit rects *inside* this widget, for widgets that paint
    /// several affordances into one rect rather than composing child
    /// widgets. Same naming convention as [`Widget::test_id`].
    ///
    /// A widget that draws its own controls (a chrome bar with a pill, a
    /// couple of icon buttons and an avatar stack) is otherwise a single
    /// addressable rect, so a harness can only reach it by guessing
    /// coordinates — and a test that guesses coordinates stops proving
    /// anything the moment the layout moves. Returning the rects the
    /// widget hit-tests against keeps the click target and the painted
    /// target the same object.
    ///
    /// Rects are reported in the same logical space as
    /// [`Widget::get_rect`]. Zero-area entries are dropped by the
    /// registry, so a control that is off screen this frame is simply
    /// not addressable.
    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> { Vec::new() }

    fn get_rect(&self) -> vello::kurbo::Rect;

    /// Resolve the OS cursor shape this widget wants while the pointer
    /// hovers `point` (in **logical** coordinates — the same space as
    /// [`Widget::get_rect`]). Return `None` to defer to the host's
    /// default arrow cursor.
    ///
    /// The default is `None`: a widget claims a cursor only by opting in.
    /// Leaf widgets override this to request a shape over their interactive
    /// area — e.g. [`TextInput`] returns [`winit::window::CursorIcon::Text`]
    /// over its editable text, [`Button`] returns `CursorIcon::Pointer`,
    /// and [`ResizeHandle`] / [`resizable_split::ResizableSplit`] return
    /// `CursorIcon::EwResize` / `NsResize` over their divider.
    ///
    /// Container widgets need not override this. The host
    /// ([`crate::app::App`]) hit-tests the whole widget tree by sweeping
    /// its `node_map` — every laid-out widget registers itself there in
    /// `build_layout`, regardless of whether it exposes [`Widget::children`]
    /// (the shell's `GroupRow` / `WindowGroup` don't) — and picks the
    /// smallest-area widget under `point` that claims a shape, i.e. the
    /// deepest / most-specific one. A container that *does* expose
    /// `children` may still walk them and return the first child's
    /// `Some(..)` to short-circuit, but it is not required to.
    fn cursor_shape(&self, _point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        None
    }

    /// The hover-tooltip text for this widget, if any — a short label revealed
    /// after a brief hover-dwell over the widget's rect (e.g. "Search" for a
    /// magnifier icon button). Return the raw string; the host resolves i18n
    /// (fluent keys are supported) and renders one themed chip on top of the
    /// whole app via [`crate::widget::tooltip::TooltipHost`].
    ///
    /// This is the toolkit's single tooltip mechanism: any widget that opts in
    /// gets a consistent, theme-aware, edge-clamped, keyboard-dismissable
    /// tooltip with **zero per-app wiring** — the host hit-tests the widget
    /// tree the same way [`cursor_shape`](Self::cursor_shape) does, so a
    /// container need not forward anything. An icon-only control should also
    /// surface the same text as its accessible name so the tooltip doubles as
    /// accessibility rather than being mouse-only. Default `None`.
    fn tooltip(&self) -> Option<&str> {
        None
    }

    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }

    /// Whether this widget is an **open modal** that should trap keyboard
    /// focus. While any widget in the tree returns `true`, the host
    /// ([`crate::app::App`]) restricts Tab/Shift+Tab and directional focus to
    /// the focusables inside this subtree — Tab from the last wraps to the
    /// first, so focus can't escape to the content behind the backdrop. The
    /// host also remembers the focused element before the trap appeared and
    /// restores it once the modal closes (this method goes back to `false`).
    ///
    /// Default `false`: ordinary widgets never trap. A modal dialog overrides
    /// it to return whether it is currently open.
    fn traps_focus(&self) -> bool { false }

    /// Local accessibility semantics for THIS widget — its semantic
    /// [`Role`](crate::a11y::Role), accessible name/description/value, and
    /// [`AccessState`](crate::a11y::AccessState).
    ///
    /// The default `None` means **presentational / pass-through**: the node is
    /// skipped and its children are reparented onto its parent, so layout-only
    /// widgets (`Row`, `Column`, `Container`, `Stack`, `Spacer`) don't pollute
    /// the tree with "group, group, group" (matching ARIA
    /// `role="presentation"`). Only widgets that carry meaning override this;
    /// semantic containers (`TabBar`, `List`, `Dialog`, `Toolbar`) return
    /// `Some(..)` and expose their structure via the existing
    /// [`children`](Widget::children).
    ///
    /// State is **derived, not stored**: compute it from existing signals
    /// ([`is_focused`](Widget::is_focused), [`is_enabled`](Widget::is_enabled),
    /// selection indices, …) at call time so it can't drift from `draw`.
    /// Geometry flows from [`get_rect`](Widget::get_rect); names must already be
    /// [`i18n!`](crate::i18n)-resolved strings, not keys. The export layer
    /// walks the tree, maps each node onto AccessKit's schema, and exports
    /// it over AT-SPI2 so standard ATs (Orca, braille) work — while an
    /// in-process channel feeds the same nodes to the built-in screen reader.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> { None }

    /// Perform an accessibility [`Action`](crate::a11y::Action) requested by an
    /// assistive technology (Orca, the built-in reader) or voice control, on
    /// THIS widget. Like [`handle_event`](Widget::handle_event) it may mutate
    /// the CRDT directly (e.g. `SetValue` splices into an Automerge `Text`).
    ///
    /// Returns `true` if the widget consumed the action. The **default
    /// returns `false`** ("not handled here") so the host
    /// ([`crate::app::App`]) can fall back to its generic dispatch —
    /// for [`Action::Focus`](crate::a11y::Action::Focus) it moves keyboard
    /// focus to the node, and for [`Action::Activate`](crate::a11y::Action::Activate)
    /// it synthesises a click at the widget's centre through the same
    /// [`UserEvent::Click`](crate::app::UserEvent::Click) path a real pointer
    /// takes. Widgets that own richer semantics (a slider's increment/decrement,
    /// a combo box's expand/collapse, a text field's set-value/set-selection)
    /// override this to handle those verbs precisely.
    ///
    /// `state`/`path` are the same CRDT handles passed to `handle_event`, so a
    /// widget can write its mutation exactly as it would for a keyboard or
    /// pointer event.
    fn perform_action(
        &mut self,
        _action: &crate::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        false
    }

    /// Whether this widget is interactive. The default is `true`.
    ///
    /// A disabled widget (`false`) must be skipped by keyboard focus
    /// traversal — `App::update_focusable_order` excludes it from
    /// `focusable_order` — and must reject input in `handle_event`
    /// (no state mutation, no CRDT write, no callback). Disabled
    /// interactive widgets also render a dimmed, no-hover/no-press
    /// look (see [`DISABLED_ALPHA`]). Implementors back this with a
    /// stored `enabled: bool` set via a `with_enabled`/`set_enabled`
    /// builder; non-interactive widgets inherit the default.
    fn is_enabled(&self) -> bool { true }
    fn needs_commit(&self) -> bool { false }
    fn on_commit(&mut self) {}
    fn get_state(&self) -> Option<String> { None }
    fn set_state(&mut self, _state: &str) {}

    /// Caret-review snapshot for the screen reader: `(caret_byte, text)` for an
    /// editable text widget that currently holds the caret, or `None` for every
    /// other widget (the default).
    ///
    /// The host ([`crate::app::App`]) reads this on the focused widget *before*
    /// and *after* an event. When the text is unchanged but the caret moved
    /// (arrow keys, Home/End), the reader announces the character or word the
    /// caret landed on — so a user can review existing content, not just typing
    /// echo. `caret_byte` is a UTF-8 byte offset into `text`, always on a char
    /// boundary.
    fn text_review(&self) -> Option<(usize, String)> { None }

    /// Deliver an out-of-band string payload injected by the host
    /// application via [`crate::app::UserEvent::External`]. The default
    /// is a no-op, so existing widgets are unaffected. Apps that drive a
    /// worker thread (e.g. the qOS installer streaming `qos-install`
    /// output back into its status log) override this to fold the
    /// payload into their state. The App requests a redraw and a re-sync
    /// after dispatching, so mutating widget/CRDT state here is enough to
    /// surface the change on the next frame.
    fn on_external_event(&mut self, _payload: &str, _state: &mut AutoCommit, _root: &ObjId) {}

    /// Handle a global back/forward navigation gesture — a two-finger
    /// horizontal swipe (touchpad) or an edge-pull swipe (touchscreen) —
    /// recognised by [`crate::gesture::NavGestureRecognizer`] in the host
    /// [`crate::app::App`] and delivered to the whole tree. Return `true`
    /// once a widget consumes it.
    ///
    /// The **default forwards to children**, so any widget that owns
    /// history (a browser's [`WebView`](crate::widget::WebView), a wizard,
    /// a pane stack) becomes swipe-navigable just by overriding this — the
    /// containers above it pass the gesture down for free, and every app
    /// built on the toolkit gets back/forward gestures without wiring up
    /// input plumbing. `state`/`path` are the same CRDT handles as
    /// [`handle_event`](Widget::handle_event).
    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        for child in self.children_mut() {
            if child.on_nav_gesture(dir, state, path) {
                return true;
            }
        }
        false
    }

    /// One-shot signal that the host should move keyboard focus to this
    /// subtree's first focusable widget after the next structural
    /// rebuild. Reading the flag MUST clear it (rising-edge semantics).
    /// Only consulted when [`crate::app::App::autofocus_on_structure_change`]
    /// is set, so the default (always `false`) leaves the shell / greeter
    /// untouched. A wizard-style app raises it on step transitions so the
    /// first field of the new step is typeable without a click.
    fn take_focus_reset(&mut self) -> bool { false }

    // --- Fullscreen / presentation contract ---
    /// The ROOT widget's desired OS-fullscreen state, polled once per frame by
    /// [`crate::app::App`]. `Some(true)` asks the host to promote the surface
    /// to a borderless full-output window; `Some(false)` asks to restore the
    /// normal windowed/tiled state; `None` (the default) means "no opinion" —
    /// the App leaves the current fullscreen state untouched.
    ///
    /// **Level-triggered, not edge-triggered.** The App compares this against
    /// the live window state each frame and only issues a winit
    /// `set_fullscreen` when they differ, so returning a steady `Some(true)`
    /// for the whole duration of (say) a slideshow is correct and idempotent.
    /// Only the root widget is consulted. Any app can opt in — a presentation
    /// app, a video player, a photo viewer — by overriding this to reflect its
    /// own "immersive" state.
    fn fullscreen_intent(&self) -> Option<bool> { None }

    /// Output + view parameters the App uses **when it promotes this root widget
    /// to fullscreen** (i.e. when [`Self::fullscreen_intent`] resolves to
    /// `Some(true)`). This is where an app names the target output — e.g.
    /// [`crate::fullscreen::FullscreenTarget::Largest`] to put a presentation /
    /// immersive video on the big screen — and whether the fullscreen surface
    /// shows a distinct presentation view or mirrors the app window.
    ///
    /// The App enumerates the live monitors and runs
    /// [`crate::fullscreen::select_output`] against this each time it enters
    /// fullscreen, so returning a steady value is correct. Default: the largest
    /// output, mirroring the window.
    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        crate::fullscreen::FullscreenRequest::mirror(crate::fullscreen::FullscreenTarget::Largest)
    }

    /// Notification from the host that the *actual* OS-fullscreen state changed
    /// (the compositor granted fullscreen, or exited it). Called on the root
    /// widget only, on the rising/falling edge. Lets an app react to a
    /// *compositor-initiated* exit — e.g. a global un-fullscreen keybind, or
    /// the window losing its fullscreen grant — by leaving its presentation
    /// layout even though the app itself never asked. Default no-op.
    fn on_fullscreen_changed(&mut self, _active: bool) {}

    /// Which **record** this widget's document should be.
    ///
    /// A record is the unit of state and of collaboration: one document, one
    /// identity, one channel (see [`crate::record`]). An app that works on one
    /// thing at a time — a document, a PDF's markup, a book's annotations —
    /// names it here, and the App checks that record's own document out before
    /// the next `sync`. So opening a second file does not overwrite the first
    /// one's state: it is a different document.
    ///
    /// Declarative, like [`Self::fullscreen_intent`]: return the record the
    /// widget is on every frame. The App compares it against the live one and
    /// only acts when they differ, so a steady value is correct and cheap.
    /// `None` — the default — means "no opinion", and the app keeps the single
    /// package-level document it always had.
    ///
    /// Only the root widget is consulted.
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> { None }

    /// The typed presentation fields this widget contributes to the App's
    /// [`View`](crate::view::View) — its scroll offset(s) and current view mode
    /// — captured on freeze (and, later, broadcast to presence) and restored on
    /// thaw via [`restore_view_state`](Self::restore_view_state).
    ///
    /// Distinct from [`get_state`](Self::get_state), which captures the
    /// *focused* widget's caret/selection: this is the structured, broadcastable
    /// presentation state a scroll container or a mode-switching root owns, and
    /// the scrollable is usually not the focused widget. Only the root widget is
    /// consulted. Default: contributes nothing.
    fn view_state(&self) -> Option<crate::view::ViewContribution> { None }

    /// Restore the presentation fields captured by
    /// [`view_state`](Self::view_state). Called on the root widget on thaw.
    /// Default: no-op.
    fn restore_view_state(&mut self, _view: &crate::view::ViewContribution) {}

    /// A widget's desired **global appearance** (theme + high-contrast +
    /// reduced-motion + UI-scale + text-scale), polled by [`crate::app::App`]
    /// each frame when [`crate::app::App::appearance_reconcile`] is enabled.
    /// This is how a settings/appearance pane makes the user's choice re-scale
    /// and re-theme the *running* app live, without the app needing a back-
    /// channel to the host: the App walks the widget tree once per tick, and
    /// the first widget returning `Some` wins.
    ///
    /// Level-triggered and idempotent — return the same value every frame while
    /// the choice is unchanged; the App only re-themes when a field actually
    /// differs from what is currently applied. Default `None` ("no opinion");
    /// containers do NOT need to forward this — the App recurses through
    /// [`Self::children`] itself.
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> { None }

    /// Told to a widget when its host hands it a record's document.
    ///
    /// The inverse of [`Self::record_intent`]: the widget names the record
    /// it wants, this names the one it got — which is also the answer for
    /// a widget that never asked and was simply given the one its tab
    /// holds.
    ///
    /// A widget that publishes presence **must** fold this into its
    /// presence key. Two instances of one app are two records now, and a
    /// key that only names the field ("the notes body") is the same key in
    /// both of them: a peer's caret arriving for one lands in whichever
    /// one drew last. Default: ignore, for the widgets that publish none.
    fn set_record_scope(&mut self, _record: &crate::record::RecordId) {}

    // --- Collab / DnD contract extensions ---
    /// Stable identity used to look up presence and permissions for this widget.
    /// Default: opt-out (no presence tracking).
    fn presence_key(&self) -> Option<PresenceKey> { None }
    /// Outbound presence this widget wants broadcast while it holds focus
    /// — typically its text caret or selection, keyed to the sub-element
    /// (e.g. a block) the caret is in. The App polls the focused widget
    /// for this after each event and broadcasts the diff so peers can
    /// render the remote caret/selection. Default: nothing to broadcast.
    fn presence_payload(
        &self,
    ) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        None
    }
    /// Whether this widget still needs `sync` to run on idle frames even
    /// without input or collab events — e.g. a collab app that hasn't yet
    /// seeded or adopted the shared document and is waiting out a grace
    /// window. The App keeps requesting redraws while this is true (and a
    /// collab socket is connected). Default: no idle sync needed.
    fn needs_idle_sync(&self) -> bool {
        false
    }
    /// Which permission scope, if any, gates this widget. Default: no gating.
    fn permission_scope(&self) -> Option<PermissionScope> { None }

    /// The tree of [`qos_actions::Resource`]s this widget currently has *open /
    /// presented*, each with the actions it exports to Ctrl+K. An app's root
    /// widget implements this to publish e.g. the playing video + its comments,
    /// or the open book + its annotations; the CRDT `doc` is passed so a
    /// provider can enumerate resources that live in the document (annotations).
    ///
    /// The [`App`](crate::app::App) collects the focused instance's tree each
    /// frame ([`App::collect_resources`](crate::app::App::collect_resources)) to
    /// scope its action palette. Default: nothing published.
    fn resources(&self, _doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        Vec::new()
    }

    /// Searchable-content sources this widget folds into the unified Ctrl+K
    /// palette, alongside its exported [`resources`](Self::resources) actions.
    /// An app declares what it can *find* (its contacts, its files, …) by
    /// returning a [`SearchProvider`](crate::search::SearchProvider) per kind;
    /// their hits appear as navigable result groups. Activating one calls
    /// [`activate_search_result`](Self::activate_search_result) with the row's
    /// opaque `action_id`. Default: nothing searchable.
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        Vec::new()
    }

    /// Navigate to the content a chosen search-provider row identifies. The
    /// `action_id` is the opaque token the widget's own
    /// [`SearchProvider`](crate::search::SearchProvider) emitted; the widget
    /// resolves it back to the record and selects / opens it. This is
    /// presentation (navigation), not a CRDT action — mutate the doc only if
    /// selecting genuinely changes shared state. Default: no-op.
    fn activate_search_result(&mut self, _action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {}

    /// The affordance-driven twin of the Ctrl+K palette: when one of a widget's
    /// own controls (a toolbar button, a context-menu item, a swatch) performs a
    /// document-mutating operation, it dispatches the SAME
    /// [`qos_actions`] action its [`ActionSpec`](qos_actions::ActionSpec) exports
    /// — applied through the executor so it replays byte-identically on peers —
    /// and reports the document heads captured **just before** that action here.
    ///
    /// The host ([`crate::app::App`]) drains this after each event and records
    /// exactly one undo boundary bracketing `[before, current]`, the same
    /// `record_committed_action` path a palette dispatch takes. So an app's own
    /// buttons and its Ctrl+K entries share one dispatch + undo path, and the
    /// user's Ctrl+Z reverts a button-driven action just like a palette one.
    ///
    /// Rising-edge: reading MUST clear it (return `Some` once per dispatched
    /// batch, then `None`). Default: nothing to record.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        None
    }

    fn accepts_drop(&self, _payload: &DragPayload) -> bool { false }
    fn on_drop(&mut self, _payload: DragPayload, _state: &mut AutoCommit, _path: &ObjId) {}
    fn wants_pointer_capture(&self) -> bool { false }

    /// Whether this child floats above its in-flow siblings: it is drawn on
    /// top of them and receives pointer events before them, but — unlike a
    /// modal overlay ([`wants_pointer_capture`]) — it does NOT swallow events
    /// that miss it, so clicks that fall outside the float still reach the
    /// content beneath. Containers use this to give a small on-video affordance
    /// (a "Skip" button) correct z-order and hit priority without deadening the
    /// player under it. Default `false`.
    fn is_floating(&self) -> bool { false }
    /// Whether this widget (or any descendant) is currently running a
    /// frame-driven animation and needs the host to keep pumping redraws
    /// until the transition settles. Default: opt-out (static widget).
    ///
    /// `App` polls this during `Event::AboutToWait` and schedules another
    /// redraw while it's true. Without this, overlays like the overview
    /// crossfade would paint one frame immediately after the click, then
    /// freeze partway through the easing curve until the next unrelated
    /// event arrived — making interactions look broken.
    fn needs_animation_frame(&self) -> bool {
        self.children().iter().any(|c| c.needs_animation_frame())
    }

    /// **Modal capture.** When this subtree contains an *open* modal
    /// overlay (a [`crate::widget::modal_dialog::ModalDialog`], a movable
    /// compose window, a blocking sheet, …) it returns `Some(rect)` where
    /// `rect` is the modal panel's box. While any widget in the tree
    /// reports `Some`, the host [`crate::app::App`]:
    ///   1. suppresses its built-in global keyboard shortcuts (Tab focus
    ///      cycling, Alt/arrow directional focus) so the modal owns the
    ///      keyboard, and
    ///   2. treats an *unhandled* pointer press as handled — swallowing
    ///      click-through so widgets painted behind the modal can't be
    ///      clicked.
    ///
    /// The modal widget itself is still responsible for consuming its own
    /// keyboard + in-rect pointer events inside `handle_event`; this query
    /// only gives the App the signal it needs to stop competing for input.
    /// Default `None` (not modal). Containers MUST forward — the default
    /// container impls do, by returning the first `Some` among children.
    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        self.children().iter().find_map(|c| c.modal_capture())
    }

    /// Returns true if this subtree mutated its structural composition (e.g.
    /// inserted/removed child widgets) since the last frame. The App polls
    /// this once per tick and rebuilds the taffy tree when true.
    ///
    /// The default returns `false`, which is correct for leaf widgets but
    /// wrong for any container that stores child `Box<dyn Widget>`s because
    /// it breaks the recursion — container widgets MUST override this to
    /// forward the query to their children:
    /// `self.children_mut().into_iter().any(|c| c.take_structure_dirty())`
    /// (or use their own child storage if `children_mut` doesn't expose
    /// them, e.g. `GroupRow::groups`).
    fn take_structure_dirty(&mut self) -> bool { false }

    /// Whether this widget's *visual* state has changed since the
    /// last successful `draw` call. The default returns `true` for
    /// back-compat — every widget that hasn't opted in is treated
    /// as dirty every frame, matching the pre-incremental-rendering
    /// behaviour. Widgets opt into incremental redraw by:
    ///   1. Tracking an internal `dirty: bool` flag.
    ///   2. Setting `dirty = true` inside `handle_event` / `sync`
    ///      whenever they mutate any field that affects `draw`.
    ///   3. Clearing the flag inside `mark_drawn()` (called by the
    ///      App after a successful `draw_with_presence`).
    ///
    /// Containers MUST override this to forward the query to their
    /// children: a container is dirty iff any descendant is.
    fn needs_redraw(&self) -> bool {
        // Recurse through children so even default-true leaves
        // composed inside a container correctly trigger a frame.
        if self.children().is_empty() {
            true
        } else {
            self.children().iter().any(|c| c.needs_redraw())
        }
    }

    /// Called by the App after this widget's scene has been written
    /// into the composed frame. Widgets clear their `dirty` flag
    /// here. Default forwards to children.
    fn mark_drawn(&mut self) {
        for c in self.children_mut() {
            c.mark_drawn();
        }
    }

    /// Whether this widget's *layout* has changed since the last
    /// successful `apply_layout`. Default true (back-compat) —
    /// matches the pre-incremental behaviour where the App ran
    /// `compute_layout` on every handled event. Widgets whose
    /// intrinsic size never changes (Button, Toggle, ToggleGroup
    /// selected-segment swap, …) should override this to return
    /// false unless an actually-size-affecting state mutation
    /// landed (e.g. TextInput's text grew).
    ///
    /// Containers MUST override to forward: a container is layout-
    /// dirty iff any descendant is.
    fn is_layout_dirty(&self) -> bool {
        if self.children().is_empty() {
            true
        } else {
            self.children().iter().any(|c| c.is_layout_dirty())
        }
    }

    /// Whether a frame that is only `layout_dirty` because of a change in
    /// this subtree needs taffy to **re-solve** intrinsic sizes
    /// (`compute_layout`), as opposed to merely re-running `apply_layout`
    /// to reposition already-solved boxes.
    ///
    /// A scroll is the motivating case: the [`Scrollable`] moves its
    /// child by translating its origin in `apply_layout`; the taffy sizes
    /// and relative positions are unchanged, so re-running the (expensive)
    /// `compute_layout` over the whole content each scroll frame is pure
    /// waste. The App consults this to skip the solve on pure-reposition
    /// frames. Default preserves the historical behaviour exactly (leaf =
    /// its `is_layout_dirty`, container = any child), so no widget changes
    /// behaviour unless it — like `Scrollable` — deliberately overrides to
    /// drop a reposition-only signal.
    fn layout_needs_measure(&self) -> bool {
        let kids = self.children();
        if kids.is_empty() {
            self.is_layout_dirty()
        } else {
            kids.iter().any(|c| c.layout_needs_measure())
        }
    }

    /// Called by `App` during draw. Widgets that consume presence/permission
    /// context can override this; the default forwards to `draw`, passing
    /// through `presence` and ignoring `_permissions`.
    ///
    /// **Spatial culling**: short-circuits when the widget's rect
    /// doesn't intersect `presence.viewport`. Overrides MUST
    /// honour this — calling [`PresenceCtx::should_cull`] at the
    /// top before doing any per-widget paint work. Off-viewport
    /// widgets stay dirty so they re-emit when scrolled back in.
    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) { return; }
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    /// **Top-layer pass.** Called by `App` (and the headless shot
    /// renderer) once per frame *after* the entire normal
    /// `draw_with_presence` recursion has finished, so anything emitted
    /// here composites ABOVE every sibling regardless of tree order.
    ///
    /// This is the toolkit's overlay mechanism: a widget whose visual
    /// extent escapes its layout box — an open [`Dropdown`] list, a
    /// popover, a menu — paints that floating content here instead of in
    /// `draw`, so a later-drawn sibling can't occlude it. The default
    /// recurses through `children_mut` (so the open widget is reached no
    /// matter how deeply it's nested) and emits nothing itself, leaving
    /// every existing widget unaffected. There is no spatial culling on
    /// this pass — overlays are small and anchored on-screen.
    ///
    /// Containers therefore need no change: as long as they expose their
    /// children via `children_mut` (which every container already does
    /// for focus/commit propagation) the recursion flows through them.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }
}

pub type Element = Box<dyn Widget>;

mod button;      pub use button::{Button, ButtonStyle, ButtonInteract, ButtonFace, face_colors};
mod checkbox;    pub use checkbox::Checkbox;
mod column;      pub use column::Column;
mod row;         pub use row::Row;
mod toggle;      pub use toggle::{Toggle, ToggleFace, ToggleInteract, toggle_face_colors};
mod stack;       pub use stack::Stack;
mod container;   pub use container::Container;
mod divider;     pub use divider::{Divider, DividerAxis};
mod link;        pub use link::Link;
mod nav_split;
pub use nav_split::{
    NavSplit, FlapState, SplitMode, SplitLayout, PushPane,
    DEFAULT_BREAKPOINT, DEFAULT_SIDEBAR_WIDTH,
};
mod app_shell;
pub use app_shell::{
    AppShell, NarrowScreen, ShellLayout,
    DEFAULT_SHELL_BREAKPOINT, DEFAULT_SHELL_SIDEBAR_WIDTH,
};
mod sidebar;
pub use sidebar::{
    SidebarDrawer, SidebarSide,
    DEFAULT_DRAWER_WIDTH, DEFAULT_DRAWER_BREAKPOINT,
};
mod nav_shell;
pub use nav_shell::{
    NavShell, ShellMode, ContentLayout, NavPane, Affordance,
    DEFAULT_NAVSHELL_BREAKPOINT, DEFAULT_NAVSHELL_SIDEBAR_WIDTH,
    DEFAULT_NAVSHELL_OVERVIEW_WIDTH,
};
mod document_viewport;
pub use document_viewport::{
    ContentDraw, DocumentViewport, SmallContentAlign, ViewportState,
    DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_PADDING, DEFAULT_PAN_STEP,
    PAN_SLOP,
};
mod floating_window;
pub use floating_window::FloatingWindow;
mod segmented;
pub use segmented::{
    TriStateControl, SegmentTone, ColorSwatchPicker, packed_color, DEFAULT_PALETTE,
};
mod text;        pub use text::Text;
mod spacer;      pub use spacer::Spacer;
mod image;       pub use image::{Image, ImageFit, ImageDecodeError};
mod slider;      pub use slider::Slider;
mod text_input;  pub use text_input::TextInput;
pub mod code_editor; pub use code_editor::{CodeEditor, Highlighter, HlState, Language, LineSpans, Span, TokenKind, token_color};
mod editable_label; pub use editable_label::{EditableLabel, EditOutcome};
pub mod autocomplete; pub use autocomplete::{Autocomplete, AutocompleteOutcome, Suggester, Suggestion};
mod number_input; pub use number_input::NumberInput;
pub mod marks; pub use marks::{Mark, MarkSpan};
pub mod markdown_input; pub use markdown_input::{BlockPrefix, InlineKind, InlineMatch, detect_block_prefix, detect_inline};
pub mod rich_text_line; pub use rich_text_line::{RichTextLine, LineSignal};
pub mod rich_text_editor; pub use rich_text_editor::{RichTextEditor, RichBlockStyle};
pub mod block_editor;   pub use block_editor::{Block, BlockEditor, BlockKind, CursorPlacement};
pub mod slash_menu;     pub use slash_menu::SlashMenu;
pub mod link_popover;   pub use link_popover::{LinkPopover, PageEntry};
pub mod page_palette;   pub use page_palette::PageJumpPalette;
pub mod page_sidebar;   pub use page_sidebar::{PageSidebar, PageSidebarEvent};
mod dropdown;    pub use dropdown::Dropdown;
pub mod tabs;    pub use tabs::{TabBar, TabDescriptor};
pub mod scrollbar; pub use scrollbar::{Scrollbar, ScrollMetrics, PressOutcome};
pub mod drag; pub use drag::{Axis as DragAxis, DragController, ListMetrics, REORDER_THRESHOLD_PX};
mod scrollable;  pub use scrollable::{Scrollable, ScrollAxis};
mod tile_host;   pub use tile_host::{TileHost, TileContent, AppKind};
mod app_widget;  pub use app_widget::{AppCommand, AppWidget, IconDescriptor};
pub mod command_palette;
pub mod context_menu;     pub use context_menu::{ContextMenu, MenuItem};
pub mod data_table;
pub mod table;         pub use table::Table;
pub mod icon_grid;     pub use icon_grid::{IconGrid, IconGridItem};
pub mod emoji_picker;  pub use emoji_picker::EmojiPicker;
mod window_group;
pub use window_group::{
    WindowGroup, WindowWidth, DEFAULT_STRIP_USABLE_PX, RESIZE_HANDLE_PX, WIDTH_FLOOR_PX,
    WIDTH_MAX_PERMILLE, WIDTH_MIN_PERMILLE, WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE,
    WIDTH_WIDE_PERMILLE, max_group_px, min_group_px, permille_for_px,
};
mod resize_handle;
pub use resize_handle::{ResizeHandle, HANDLE_HIT_HALF_PX, HANDLE_HOVER_PX, HANDLE_SLOT_PX, HANDLE_VISIBLE_PX};
mod group_row;
pub use group_row::{CrossStripDetach, GroupRow, OutputSpan};
mod strip_view;
pub use strip_view::{StripView, STRIP_COOLDOWN_MS};
mod workspace_view;
pub use workspace_view::{WorkspaceView, WorkspaceSwitchDirection};
pub mod tooltip;
pub use tooltip::{Tooltip, TooltipHost};
pub mod toast;
pub mod permission_prompt;
pub use permission_prompt::{PermissionPrompt, PromptChoice, PROMPT_HEIGHT};
pub mod file_picker;
pub use file_picker::{
    FilePicker, FilePickerEntry, FilePickerMode, FilePickerOutcome,
};

// === Widget expansion plan (2026-05-22) =============================
// These 16 modules are filled in by the per-unit dispatch agents.
// Until each agent commits, the modules expose stub types (`pub struct Foo;`)
// so the crate compiles. Public re-exports here let downstream code and
// tests reach the widgets through the canonical `ui::widget::*` path
// regardless of which agent has completed.
pub mod progress_bar;     pub use progress_bar::ProgressBar;
pub mod spinner;          pub use spinner::Spinner;
pub mod date_picker;      pub use date_picker::DatePicker;
pub mod time_picker;      pub use time_picker::TimePicker;
pub mod color_picker;     pub use color_picker::ColorPicker;
pub mod popover_widget;   pub use popover_widget::PopoverWidget;
pub mod popover;          pub use popover::Popover;
pub mod modal_dialog;     pub use modal_dialog::ModalDialog;
pub mod modal_prompt;     pub use modal_prompt::PromptBody;
pub mod toolbar;          pub use toolbar::Toolbar;
pub mod breadcrumb;       pub use breadcrumb::Breadcrumb;
pub mod avatar;           pub use avatar::Avatar;
pub mod badge;            pub use badge::Badge;
pub mod icon;             pub use icon::IconWidget;
pub mod chart;            pub use chart::{Chart, Series, Sparkline};
pub mod gauge;            pub use gauge::{RingGauge, BarGauge, GaugeThresholds};
pub mod resizable_split;  pub use resizable_split::ResizableSplit;
pub mod video_player;     pub use video_player::VideoPlayer;
pub mod webview;          pub use webview::WebView;
pub mod image_viewer;     pub use image_viewer::ImageViewer;
pub mod tree_view;        pub use tree_view::TreeView;
pub mod toggle_group;     pub use toggle_group::{ToggleGroup, SegmentedFace, SegmentedStyle, segment_colors};

// === Shared app patterns promoted from the app fleet + new primitives ======
// Widgets that many apps hand-rolled (empty states, search boxes, stat tiles)
// or asked for (semantic inline banners, filter chips). The visual, non-focus
// ones (EmptyState / Banner / StatTile) each also expose an immediate-mode
// `paint(...)` entry point so the ~20 apps that render straight to a `Scene`
// (rather than building a widget tree) can adopt them too.
pub mod empty_state;      pub use empty_state::EmptyState;
pub mod search_field;     pub use search_field::SearchField;
pub mod search_results;   pub use search_results::SearchResultsView;
pub mod stat_tile;        pub use stat_tile::{StatTile, Trend};
pub mod banner;           pub use banner::{Banner, BannerKind};
pub mod chip;             pub use chip::Chip;
pub mod card;             pub use card::{Card, CardGrid, card_columns, card_column_width};

// === Phase 1: Audit timeline + history scrubber =====================
// Pure widget + types work in `ui/` — no shell wiring yet. See
// `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
// Phase 1 ("Audit timeline + soft-delete + restore-from-history").
pub mod history;
pub mod history_group;
pub mod name_field;
pub mod activity_timeline;
pub mod history_scrubber;
pub use history::{
    HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryFilter, RedactionDisplay,
    format_relative_time, hue_to_rgb, op_class_glyph, op_class_is_revertible,
};
pub use activity_timeline::ActivityTimeline;
pub use history_scrubber::HistoryScrubber;
// The op/artifact vocabulary a `HistoryEntry` is written in. Re-exported so a
// caller can build one without naming the collab-types crate directly.
pub use collab_types::{ArtifactKind, OpClass};

// === Phase 5: Channel browser =======================================
// Pure widget work in `ui/` — wheel-disc spoke overflow integration
// lives in `shell` and is blocked on shell ABI drift. See
// `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
// Phase 5 ("Channel browser + wheel-disc overflow").
pub mod channel_browser;
pub use channel_browser::{ChannelBrowser, ChannelEntry};

// === Phase 4: Conflict toolkit ======================================
// Pure widget + types work in `ui/` — no shell wiring yet. See
// `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
// Phase 4 ("Conflict toolkit + reconciliation UI").
pub mod merge_strategy;
pub mod conflict_banner;
pub mod conflict_picker;

// === Phase 8: Threshold-shared identity + invite ceremony ===========
// The user-facing ceremony from
// `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
// Phase 8 ("Threshold-shared identity + invite ceremony"). Wraps the
// `threshold-identity` engine in a six-screen ceremony plus a QR
// widget. See `identity_flow/mod.rs` for the state-machine entry
// point.
pub mod qr;
pub mod identity_flow;
pub use qr::QrWidget;
pub use identity_flow::{
    IdentityFlow, IdentityFlowScreen, IdentityRecord, IdentityStore,
    InviteEnvelope, InviteExpiry, MemoryIdentityStore, PairingHandshake,
    PairingPayload, ScreenOutcome,
};
pub use merge_strategy::{
    Alternative, ConcurrentOp, ConflictKind, ConflictResolver, MergeStrategy,
    surface_banner,
};
pub use conflict_banner::ConflictBanner;
pub use conflict_picker::{ConflictPicker, PickerOutcome};

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod a11y_tree_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// `Widget::test_id` defaults to `None`. New widgets that don't opt
    /// in to a stable test identifier inherit this — guarantees a path-
    /// based fallback in `ShellDriver::widget_registry` rather than a
    /// hard-coded panic. Pinned so a future edit that removes the default
    /// blanket-breaks every widget impl that hasn't been touched yet.
    #[test]
    fn widget_test_id_default_is_none() {
        // `Spacer` is the simplest widget that lives in the crate and
        // doesn't override `test_id` — fits the "inherits the default"
        // assertion exactly.
        let s = Spacer::new();
        assert!(
            Widget::test_id(&s).is_none(),
            "default Widget::test_id should be None"
        );
    }
}
