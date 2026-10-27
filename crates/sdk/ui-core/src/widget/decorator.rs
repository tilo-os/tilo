//! Extending a widget without re-implementing it: the [`Decorator`] trait.
//!
//! A widget that adds one behaviour to another — a diagnostic probe, an
//! app root that indexes its content for search, a surface host that
//! refreshes what it presents — should cost the hook it adds, not a hundred
//! lines of forwarding that silently go stale whenever [`Widget`] grows.
//!
//! Implement [`Decorator`] instead of [`Widget`]: name the wrapped widget,
//! give the two accessors, and override only what changes. Every [`Widget`]
//! method has a default here that forwards to the inner widget, and a
//! blanket impl makes every decorator a [`Widget`], so it can be boxed as an
//! [`Element`](super::Element), take part in any traversal, and implement
//! further traits that require [`Widget`] (such as
//! [`AppWidget`](super::AppWidget)).
//!
//! ```ignore
//! struct Probe { content: Column }
//!
//! impl ui::widget::Decorator for Probe {
//!     type Inner = Column;
//!     fn inner(&self) -> &Column { &self.content }
//!     fn inner_mut(&mut self) -> &mut Column { &mut self.content }
//!
//!     fn native_input_snapshot(&self, doc: &AutoCommit, path: &ObjId) -> Option<serde_json::Value> {
//!         Some(serde_json::json!({ "rect": format!("{:?}", self.content.get_rect()) }))
//!     }
//! }
//! ```
//!
//! # A decorator is its inner widget plus overrides
//!
//! Nothing is forwarded "partly": [`children`](Widget::children) forwards
//! too, so a host traversal meets the decorator in the inner widget's place
//! and visits every node exactly once. The only methods that describe the
//! decorator rather than the inner widget are the identity ones:
//! `concrete_type_name` names the decorator's type and
//! [`as_any`](Widget::as_any) and [`as_any_mut`](Widget::as_any_mut)
//! downcast to the decorator itself unless overridden.
//!
//! A boxed [`Element`](super::Element) is wrapped with `type Inner = dyn
//! Widget;` and accessors returning `&(dyn Widget + 'static)`.
//!
//! Four consequences are worth knowing:
//!
//! * **An override replaces the inner widget's answer.** Because the inner
//!   widget is not visited separately, a decorator that adds to a
//!   collecting hook (`resources`, `search_providers`,
//!   `take_control_invocation`) includes the inner widget's answer:
//!   `let mut all = mine; all.extend(self.inner().resources(doc)); all`.
//! * **Override the host's entry point.** Some [`Widget`] defaults are
//!   derived from another method (`tooltip_target_at` calls `tooltip`,
//!   `collect_damage` calls `needs_redraw`). Here each forwards to the inner
//!   widget's own method, so an override of the derived-from method alone is
//!   not what the host calls: override the entry point the host uses.
//!   Painting and input each have a single entry point,
//!   [`draw`](Widget::draw) and [`handle_event`](Widget::handle_event).
//! * **The decorator stands in for its inner widget's layout node.** The
//!   inner widget registers itself in the node map during `build_layout`;
//!   the decorator then takes that entry, so a host that reaches a widget
//!   through the map (an accessibility action, focused input, a cursor
//!   shape, a tooltip, undo status) meets the decorator in the inner
//!   widget's place, and everything it does not override still reaches the
//!   inner widget. A node the inner widget handed to a widget of its own (a
//!   field it lays out as itself) stays that widget's. A decorator that
//!   builds a node of its own around the inner one registers itself for it
//!   with [`register_node`].
//! * **Method calls.** A decorator is both a [`Decorator`] and, through the
//!   blanket impl, a [`Widget`]. With both traits imported, `self.get_rect()`
//!   is ambiguous, so implement the trait by path (`impl
//!   ui::widget::Decorator for ..`) rather than importing it; the prelude
//!   deliberately does not export it.

use std::collections::HashMap;

use super::{AppModel, ModelPath, capability};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use super::{
    EventCtx, LayoutInfo, ManualPolicy, PrimaryScrollAction, PrimaryScrollRegion,
    PrimaryScrollResult, Widget,
};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;

/// A widget as `&dyn Widget`, also one that already is a trait object: a
/// decorator's inner widget may be either, and its layout walk hands the
/// inner widget on ([`Widget::for_each_layout_child`]).
pub trait AsDynWidget {
    /// This widget as a trait object.
    fn as_dyn_widget(&self) -> &dyn Widget;
}

impl<T: Widget> AsDynWidget for T {
    fn as_dyn_widget(&self) -> &dyn Widget {
        self
    }
}

impl AsDynWidget for dyn Widget {
    fn as_dyn_widget(&self) -> &dyn Widget {
        self
    }
}

impl AsDynWidget for dyn super::AppWidget {
    fn as_dyn_widget(&self) -> &dyn Widget {
        self
    }
}

/// Point `node` at `widget` in the host's node map, replacing the inner
/// widget's own registration. For a [`Decorator`] that overrides a hook the
/// host reaches through the node map (a cursor shape, a tooltip, undo
/// status); call it from `build_layout` after the inner widget built its
/// node.
pub fn register_node<W: Widget + 'static, S: std::hash::BuildHasher>(
    node_map: &mut HashMap<NodeId, *mut dyn Widget, S>,
    node: NodeId,
    widget: &mut W,
) {
    node_map.insert(node, std::ptr::from_mut::<dyn Widget>(widget));
}

/// Point `node` at `outer` when the inner widget whose address and type are
/// `inner` registered itself for it: the decorator stands in for its inner
/// widget.
fn stand_in(
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    inner: (*const (), &'static str),
    outer: &mut (dyn Widget + 'static),
) {
    let Some(&registered) = node_map.get(&node) else {
        return;
    };
    // SAFETY: `registered` was put in the map by the build_layout that just
    // returned, for a widget inside `outer` that lives as long as this frame's
    // layout; reading its type name neither retains nor mutates it.
    let registered_type = unsafe { (*registered).concrete_type_name() };
    if std::ptr::eq(registered.cast::<()>().cast_const(), inner.0) && registered_type == inner.1 {
        node_map.insert(node, std::ptr::from_mut(outer));
    }
}

/// Declares [`Decorator`] and its blanket [`Widget`] impl from one list of
/// forwarded methods, so forwarding a method is one line here rather than
/// two copies that can drift apart. Each method under `ref` or `mut` becomes
/// a [`Decorator`] default that forwards to the inner widget and a
/// [`Widget`] method that calls the decorator's own. Methods that do
/// anything else are written out under `trait` and `impl`.
macro_rules! decorator_trait {
    (
        $d:tt
        $(#[$trait_attr:meta])*
        trait { $($trait_items:tt)* }
        $(#[$impl_attr:meta])*
        impl { $($impl_items:tt)* }
        ref { $(
            $(#[$ref_attr:meta])*
            fn $ref:ident($($ref_arg:ident: $ref_ty:ty),* $(,)?) $(-> $ref_ret:ty)?;
        )* }
        mut { $(
            $(#[$mut_attr:meta])*
            fn $mut:ident($($mut_arg:ident: $mut_ty:ty),* $(,)?) $(-> $mut_ret:ty)?;
        )* }
    ) => {
        $(#[$trait_attr])*
        pub trait Decorator: Sized + 'static {
            $($trait_items)*
            $(
                $(#[$ref_attr])*
                fn $ref(&self, $($ref_arg: $ref_ty),*) $(-> $ref_ret)? {
                    self.inner().$ref($($ref_arg),*)
                }
            )*
            $(
                $(#[$mut_attr])*
                fn $mut(&mut self, $($mut_arg: $mut_ty),*) $(-> $mut_ret)? {
                    self.inner_mut().$mut($($mut_arg),*)
                }
            )*
        }

        $(#[$impl_attr])*
        impl<T: Decorator> Widget for T {
            $($impl_items)*
            $(
                $(#[$ref_attr])*
                fn $ref(&self, $($ref_arg: $ref_ty),*) $(-> $ref_ret)? {
                    Decorator::$ref(self, $($ref_arg),*)
                }
            )*
            $(
                $(#[$mut_attr])*
                fn $mut(&mut self, $($mut_arg: $mut_ty),*) $(-> $mut_ret)? {
                    Decorator::$mut(self, $($mut_arg),*)
                }
            )*
        }

        /// The same trait for the toolkit's own decorators that apps box
        /// (`concrete_decorator_widget`): it has no blanket [`Widget`]
        /// impl, so each such type gets a concrete one, compiled once into
        /// the toolkit. Through the blanket impl a decorator's [`Widget`]
        /// methods are generic, and every app that boxes one compiled them
        /// again, with the inner widget they forward to.
        pub mod concrete {
            #[allow(
                clippy::wildcard_imports,
                reason = "the trait is the parent's, item for item, and names what it imports"
            )]
            use super::*;

            $(#[$trait_attr])*
            pub trait Decorator: Sized + 'static {
                $($trait_items)*
                $(
                    $(#[$ref_attr])*
                    fn $ref(&self, $($ref_arg: $ref_ty),*) $(-> $ref_ret)? {
                        self.inner().$ref($($ref_arg),*)
                    }
                )*
                $(
                    $(#[$mut_attr])*
                    fn $mut(&mut self, $($mut_arg: $mut_ty),*) $(-> $mut_ret)? {
                        self.inner_mut().$mut($($mut_arg),*)
                    }
                )*
            }
        }

        /// [`Widget`] for `$ty`, a toolkit type that implements
        /// [`concrete::Decorator`]: the blanket impl's methods, written for
        /// the one type, so they are compiled here and every app links them
        /// from the shared runtime instead of compiling its own copy.
        macro_rules! concrete_decorator_widget {
            ($d ty:ty) => {
                const _: () = {
                    use crate::widget::decorator::concrete::Decorator;

                    impl Widget for $d ty {
                        $($impl_items)*
                        $(
                            $(#[$ref_attr])*
                            fn $ref(&self, $($ref_arg: $ref_ty),*) $(-> $ref_ret)? {
                                Decorator::$ref(self, $($ref_arg),*)
                            }
                        )*
                        $(
                            $(#[$mut_attr])*
                            fn $mut(&mut self, $($mut_arg: $mut_ty),*) $(-> $mut_ret)? {
                                Decorator::$mut(self, $($mut_arg),*)
                            }
                        )*
                    }
                };
            };
        }
    };
}

decorator_trait! {
    $
    /// A widget that extends another by overriding a few [`Widget`] methods and
    /// forwarding the rest. See the [module docs](self).
    trait {
        /// The widget this one extends.
        type Inner: Widget + AsDynWidget + ?Sized;

        /// The extended widget.
        fn inner(&self) -> &Self::Inner;

        /// The extended widget, mutably.
        fn inner_mut(&mut self) -> &mut Self::Inner;

        #[doc(hidden)]
        fn concrete_type_name(&self) -> &'static str {
            std::any::type_name::<Self>()
        }

        /// The inner widget itself: the decorator's layout node stands for
        /// it, so a retained layout is checked through the inner widget and
        /// what it lays out. A decorator that lays out more than its inner
        /// widget (an overlay it mounts) lists that too.
        fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
            visitor(self.inner().as_dyn_widget());
            true
        }

        fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>) {
            self.inner().provide(request);
        }

        fn provide_mut<'a>(&'a mut self, request: &mut capability::RequestMut<'a, '_>) {
            self.inner_mut().provide_mut(request);
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.inner_mut().build_layout(taffy, theme, node_map)
        }

        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        fn stands_for(&self, address: *const ()) -> bool {
            std::ptr::from_ref(self).cast::<()>() == address || self.inner().stands_for(address)
        }

        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }

        /// A decorator's structure flag is its own [`Decorator::take_structure_dirty`]
        /// (which may add flags of its own), so by default it says
        /// [`crate::widget::StructureChange::Rebuild`]; one that adds no flags forwards
        /// its inner widget's change by overriding this.
        fn take_structure_change(&mut self) -> crate::widget::StructureChange {
            crate::widget::StructureChange::of(Decorator::take_structure_dirty(self))
        }

        /// A decorator composes from its allocation when anything it wraps does.
        fn composition_follows_allocation(&self) -> bool {
            crate::widget::layout::tree_composition_follows_allocation(self.inner())
        }

        /// Decorators must opt in themselves: an outer handler can change what
        /// its inner widget declares even when the inner scroll is view-only.
        fn view_only_scroll(&self, _event: &WindowEvent) -> bool {
            false
        }

        /// Read from this decorator's own node: one that overrides
        /// `accessibility` may hide (or reveal) what its inner widget reports.
        fn accessibility_hidden(&self) -> bool {
            Decorator::accessibility(self).is_some_and(|node| node.state.hidden)
        }

        /// Not forwarded: a decorator may change what its inner widget
        /// reports through `accessibility`, so by default the tree reads
        /// that. One that shares its children overrides both.
        fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
            None
        }

        /// The decorator's own node without its children. Not forwarded: a
        /// decorator that rewrites `accessibility` would otherwise answer
        /// with its inner widget's root. One that only adds children may
        /// forward to its inner widget's root itself.
        fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
            Decorator::accessibility(self).map(|mut node| {
                node.children.clear();
                node
            })
        }
    }

    /// Every decorator is a widget: each method is the decorator's own, which
    /// forwards to the inner widget unless the decorator overrides it.
    impl {
        #[doc(hidden)]
        fn concrete_type_name(&self) -> &'static str {
            Decorator::concrete_type_name(self)
        }

        fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
            Decorator::for_each_layout_child(self, visitor)
        }

        fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>) {
            Decorator::provide(self, request);
        }

        fn provide_mut<'a>(&'a mut self, request: &mut capability::RequestMut<'a, '_>) {
            Decorator::provide_mut(self, request);
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = Decorator::build_layout(self, taffy, theme, node_map);
            let inner = (
                std::ptr::from_ref(self.inner()).cast::<()>(),
                self.inner().concrete_type_name(),
            );
            stand_in(node_map, node, inner, self);
            node
        }

        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Decorator::as_any(self)
        }

        fn stands_for(&self, address: *const ()) -> bool {
            Decorator::stands_for(self, address)
        }

        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Decorator::as_any_mut(self)
        }

        fn take_structure_change(&mut self) -> crate::widget::StructureChange {
            Decorator::take_structure_change(self)
        }

        fn composition_follows_allocation(&self) -> bool {
            Decorator::composition_follows_allocation(self)
        }

        fn view_only_scroll(&self, event: &WindowEvent) -> bool {
            Decorator::view_only_scroll(self, event)
        }

        fn accessibility_hidden(&self) -> bool {
            Decorator::accessibility_hidden(self)
        }

        fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
            Decorator::accessibility_shared(self)
        }

        fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
            Decorator::accessibility_root(self)
        }
    }

    ref {
        fn focus_reveal_context() -> Option<vello::kurbo::Rect>;
        #[doc(hidden)]
        fn resets_callback_binding() -> bool;
        #[doc(hidden)]
        fn legacy_callback_boundary() -> Option<(NodeId, crate::history_mode::HistoryOwner)>;
        #[doc(hidden)]
        fn inactive_callback_boundary() -> Option<NodeId>;
        fn native_input_snapshot(state: &AppModel, path: &ModelPath) -> Option<serde_json::Value>;
        fn callback_binding() -> Option<crate::bound_dispatch::CallbackBinding>;
        fn has_separate_document_owner() -> bool;
        fn history_owner() -> Option<crate::history_mode::HistoryOwner>;
        fn record_owners() -> Vec<crate::history_mode::HistoryOwner>;
        #[doc(hidden)]
        fn manual_layout_policy() -> Option<ManualPolicy>;
        fn layout_info() -> Option<LayoutInfo>;
        fn children() -> Vec<&dyn Widget>;
        fn for_each_child(visitor: &mut dyn FnMut(&dyn Widget));
        fn for_each_child_in_clip(visitor: &mut dyn FnMut(&dyn Widget));
        fn name() -> &str;
        fn test_id() -> Option<&str>;
        fn window_slot(id: String) -> Option<crate::window_toolbar::ToolbarAction>;
        fn test_rects() -> Vec<(String, vello::kurbo::Rect)>;
        fn get_rect() -> vello::kurbo::Rect;
        fn paint_bounds() -> vello::kurbo::Rect;
        fn child_clip_rect() -> Option<vello::kurbo::Rect>;
        fn cursor_shape(point: vello::kurbo::Point) -> Option<winit::window::CursorIcon>;
        fn claims_raw_touch(point: vello::kurbo::Point) -> bool;
        fn touch_claim(point: vello::kurbo::Point) -> crate::interaction::TouchClaim;
        fn tooltip() -> Option<&str>;
        fn tooltip_side() -> crate::widget::tooltip::TooltipSide;
        fn tooltip_target_at(point: vello::kurbo::Point) -> Option<(vello::kurbo::Rect, String)>;
        fn is_focusable() -> bool;
        fn is_focused() -> bool;
        fn wants_tab_input() -> bool;
        fn ime_cursor_rect() -> Option<vello::kurbo::Rect>;
        fn traps_focus() -> bool;
        fn scopes_keyboard_focus() -> bool;
        fn accessibility() -> Option<crate::a11y::AccessNode>;
        fn synthetic_accessibility_rect(path: &[usize]) -> Option<vello::kurbo::Rect>;
        fn accessibility_child_parent(child_index: usize) -> Option<usize>;
        fn accessibility_child_path(child_index: usize) -> Option<Vec<usize>>;
        fn accessibility_represents_child(child_index: usize) -> bool;
        fn accessibility_child_description(child_index: usize) -> Option<String>;
        fn is_enabled() -> bool;
        fn ime_requires_deferred_ack() -> bool;
        fn ime_refuses_ordinary_commits() -> bool;
        fn ime_edit_fingerprint(key: &[u8; 32]) -> Option<[u8; 32]>;
        fn ime_content() -> Option<crate::ime::InputContent>;
        fn ime_multiline() -> bool;
        fn presented() -> bool;
        fn arrives_from() -> crate::arriving::Source;
        fn arrival_progress() -> Option<crate::arriving::Progress>;
        fn arrival_shape() -> crate::widget::SkeletonShape;
        fn text_review() -> Option<(usize, String)>;
        fn nav_availability() -> crate::gesture::NavAvailability;
        fn dev_route() -> Option<crate::widget::router::RouteSnapshot>;
        fn appearance_intent() -> Option<crate::theme::AppearanceIntent>;
        fn minimum_window_width() -> Option<f64>;
        fn needs_idle_sync() -> bool;
        fn wants_pointer_capture() -> bool;
        fn disclosing() -> Option<crate::widget::Disclosing>;
        fn is_band() -> bool;
        fn animation_detail() -> Option<String>;
        fn needs_animation_frame() -> bool;
        fn next_wake_deadline() -> Option<Instant>;
        fn holds_frame_until(now: Instant) -> Option<Instant>;
        fn modal_capture() -> Option<vello::kurbo::Rect>;
        fn has_pending_visual_work() -> bool;
        fn has_transient_feedback() -> bool;
        fn needs_redraw() -> bool;
        fn collect_damage(regions: &mut Vec<vello::kurbo::Rect>);
        fn damage_covers_own_box() -> bool;
        fn is_layout_dirty() -> bool;
        fn layout_needs_measure() -> bool;
        #[doc(hidden)]
        fn relaid_layout_node() -> Option<NodeId>;
        fn is_default_keyboard_focus() -> bool;
        fn keyboard_input_scope() -> Option<crate::application::InstanceId>;
        fn primary_vertical_scroll() -> Option<PrimaryScrollRegion>;
        #[doc(hidden)]
        #[allow(
            private_interfaces,
            reason = "finite toolkit resource ownership requests"
        )]
        fn visit_resource_owners(
            request: &mut crate::bound_dispatch::legacy_document::ResourceReadRequest<'_>,
        ) -> Result<(), tilo_actions::DoorError>;
    }

    mut {
        fn set_full_bleed(full_bleed: bool);
        fn place_disclosing(slot: vello::kurbo::Rect);
        fn note_placement(placement: crate::widget::Placement);
        #[doc(hidden)]
        #[allow(
            private_interfaces,
            reason = "opaque toolkit-only request prevents a public root/registry decoder loan"
        )]
        fn visit_components(
            request: &mut crate::bound_dispatch::ComponentRequest<'_>,
        ) -> Result<(), crate::bound_dispatch::BoundDispatchError>;
        #[doc(hidden)]
        fn visit_retained_components(
            request: &mut crate::bound_dispatch::RetainedComponentRequest<'_, '_>,
        ) -> Result<(), crate::bound_dispatch::BoundDispatchError>;
        fn set_platform_conventions(conventions: crate::platform::PlatformConventions);
        fn set_interaction_environment(environment: crate::interaction::InteractionEnvironment);
        fn sync(state: &AppModel, path: &ModelPath);
        fn update(state: &AppModel, path: &ModelPath) -> bool;
        fn serve_deadline(now: Instant) -> bool;
        fn settle(edits: &mut crate::widget::Edits<'_>, path: &automerge::ObjId);
        fn measure(
            known: taffy::Size<Option<f32>>,
            available: taffy::Size<taffy::AvailableSpace>,
            text_renderer: &mut TextRenderer,
        ) -> taffy::Size<f32>;
        fn adjust_layout_style(
            taffy: &mut TaffyTree<()>,
            node: NodeId,
            text: &mut TextRenderer,
        ) -> bool;
        fn apply_layout(taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point);
        fn draw(
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        );
        fn observe_physical_input(input: crate::widget::PhysicalInput);
        fn handle_event(event: &WindowEvent, ctx: &mut EventCtx<'_>);
        fn children_mut() -> Vec<&mut (dyn Widget + '_)>;
        fn reveal_descendant(target: *const dyn Widget) -> bool;
        fn take_action_palette_request() -> Option<tilo_actions::ActionAddress>;
        fn take_search_request() -> bool;
        fn set_focus(focused: bool);
        fn set_invalid(invalid: bool);
        fn focus_synthetic_accessibility(path: &[usize]) -> bool;
        fn perform_action(
            action: &crate::a11y::Action,
            key_path: &[u64],
            state: &mut crate::widget::Edits<'_>,
            path: &ModelPath,
        ) -> bool;
        fn perform_semantic_action(
            target: &str,
            action: &crate::a11y::Action,
            state: &mut crate::widget::Edits<'_>,
            path: &ModelPath,
        ) -> bool;
        fn start_deferred_text() -> Option<crate::webview::text_input::Receipt>;
        fn commit_deferred_text(text: &str) -> Option<crate::webview::text_input::Receipt>;
        fn on_external_event(payload: &str, state: &mut crate::widget::Edits<'_>, root: &ModelPath);
        fn on_nav_gesture(
            dir: crate::gesture::NavDirection,
            state: &AppModel,
            path: &ModelPath,
        ) -> bool;
        fn take_focus_reset() -> bool;
        fn take_accessibility_request() -> Option<crate::a11y::AccessibilityRequest>;
        fn take_app_activation() -> Option<crate::application::InstanceId>;
        fn restore_dev_route(route: &crate::widget::router::RouteSnapshot) -> bool;
        fn set_motion_reduced(reduced: bool);
        fn apply_room_action(
            doc: &mut crate::widget::Edits<'_>,
            type_id: &str,
            payload: &[u8],
        ) -> bool;
        fn take_structure_dirty() -> bool;
        fn remount_children(remount: &mut crate::widget::Remount<'_>) -> bool;
        fn mark_drawn();
        fn release_scene_cache();
        fn draw_overlay(
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        );
        fn apply_primary_vertical_scroll(action: PrimaryScrollAction) -> PrimaryScrollResult;
        fn focus_requested(target: Option<NodeId>, tree: &TaffyTree<()>);
        fn mount_native_views(host: &crate::widget::native_view::NativeViewHost<'_>);
        fn native_view_frame_start();
        fn unmount_native_views();
        #[doc(hidden)]
        #[allow(
            private_interfaces,
            reason = "finite toolkit resource ownership requests"
        )]
        fn visit_resource_owners_mut(
            request: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
        ) -> Result<(), tilo_actions::DoorError>;
    }
}

// The toolkit's decorators that apps box themselves. A type here implements
// `concrete::Decorator` instead of `Decorator`.
concrete_decorator_widget!(super::nav_list::NavList);
concrete_decorator_widget!(super::media_transport::MediaTransport);

#[cfg(test)]
#[path = "decorator_tests.rs"]
mod tests;
