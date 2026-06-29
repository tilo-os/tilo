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
    fn get_rect(&self) -> vello::kurbo::Rect;
    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { false }
    fn on_commit(&mut self) {}
    fn get_state(&self) -> Option<String> { None }
    fn set_state(&mut self, _state: &str) {}

    /// Deliver an out-of-band string payload injected by the host
    /// application via [`crate::app::UserEvent::External`]. The default
    /// is a no-op, so existing widgets are unaffected. Apps that drive a
    /// worker thread (e.g. the qOS installer streaming `qos-install`
    /// output back into its status log) override this to fold the
    /// payload into their state. The App requests a redraw and a re-sync
    /// after dispatching, so mutating widget/CRDT state here is enough to
    /// surface the change on the next frame.
    fn on_external_event(&mut self, _payload: &str, _state: &mut AutoCommit, _root: &ObjId) {}

    /// One-shot signal that the host should move keyboard focus to this
    /// subtree's first focusable widget after the next structural
    /// rebuild. Reading the flag MUST clear it (rising-edge semantics).
    /// Only consulted when [`crate::app::App::autofocus_on_structure_change`]
    /// is set, so the default (always `false`) leaves the shell / greeter
    /// untouched. A wizard-style app raises it on step transitions so the
    /// first field of the new step is typeable without a click.
    fn take_focus_reset(&mut self) -> bool { false }

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
    fn accepts_drop(&self, _payload: &DragPayload) -> bool { false }
    fn on_drop(&mut self, _payload: DragPayload, _state: &mut AutoCommit, _path: &ObjId) {}
    fn wants_pointer_capture(&self) -> bool { false }
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

mod button;      pub use button::Button;
mod checkbox;    pub use checkbox::Checkbox;
mod column;      pub use column::Column;
mod row;         pub use row::Row;
mod toggle;      pub use toggle::Toggle;
mod stack;       pub use stack::Stack;
mod container;   pub use container::Container;
mod text;        pub use text::Text;
mod spacer;      pub use spacer::Spacer;
mod image;       pub use image::{Image, ImageFit, ImageDecodeError};
mod slider;      pub use slider::Slider;
mod text_input;  pub use text_input::TextInput;
pub mod marks; pub use marks::{Mark, MarkSpan};
pub mod rich_text_line; pub use rich_text_line::{RichTextLine, LineSignal};
pub mod block_editor;   pub use block_editor::{Block, BlockEditor, BlockKind, CursorPlacement};
pub mod slash_menu;     pub use slash_menu::SlashMenu;
pub mod link_popover;   pub use link_popover::{LinkPopover, PageEntry};
pub mod page_palette;   pub use page_palette::PageJumpPalette;
mod dropdown;    pub use dropdown::Dropdown;
pub mod tabs;    pub use tabs::{TabBar, TabDescriptor};
mod scrollable;  pub use scrollable::{Scrollable, ScrollAxis};
mod tile_host;   pub use tile_host::{TileHost, TileContent, AppKind};
mod app_widget;  pub use app_widget::{AppCommand, AppWidget, IconDescriptor};
pub mod command_palette;
pub mod context_menu;
pub mod data_table;
mod window_group;
pub use window_group::{WindowGroup, WindowWidth, WIDTH_MIN_PX, WIDTH_MAX_PX, RESIZE_HANDLE_PX};
mod resize_handle;
pub use resize_handle::{ResizeHandle, HANDLE_HIT_HALF_PX, HANDLE_HOVER_PX, HANDLE_VISIBLE_PX};
mod group_row;
pub use group_row::{CrossStripDetach, GroupRow};
mod strip_view;
pub use strip_view::{StripView, STRIP_COOLDOWN_MS};
mod workspace_view;
pub use workspace_view::{WorkspaceView, WorkspaceSwitchDirection};
pub mod tooltip;
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
pub mod modal_dialog;     pub use modal_dialog::ModalDialog;
pub mod toolbar;          pub use toolbar::Toolbar;
pub mod breadcrumb;       pub use breadcrumb::Breadcrumb;
pub mod avatar;           pub use avatar::Avatar;
pub mod badge;            pub use badge::Badge;
pub mod resizable_split;  pub use resizable_split::ResizableSplit;
pub mod video_player;     pub use video_player::VideoPlayer;
pub mod image_viewer;     pub use image_viewer::ImageViewer;
pub mod tree_view;        pub use tree_view::TreeView;
pub mod toggle_group;     pub use toggle_group::ToggleGroup;

// === Phase 1: Audit timeline + history scrubber =====================
// Pure widget + types work in `ui/` — no shell wiring yet. See
// `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
// Phase 1 ("Audit timeline + soft-delete + restore-from-history").
pub mod history;
pub mod activity_timeline;
pub mod history_scrubber;
pub use history::{
    HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryFilter,
    format_relative_time, hue_to_rgb, op_class_glyph, op_class_is_revertible,
};
pub use activity_timeline::ActivityTimeline;
pub use history_scrubber::HistoryScrubber;

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
