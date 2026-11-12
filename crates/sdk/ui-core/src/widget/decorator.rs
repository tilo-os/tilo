//! Extending a widget without re-implementing it: the [`Decorator`] trait.
//!
//! A widget that adds one behaviour to another (a diagnostic probe, an app root that indexes
//! its content for search) should cost the hook it adds, not a hundred lines of forwarding
//! that go stale whenever [`Widget`] grows. Implement [`Decorator`] instead: name the wrapped
//! widget, give the two accessors, override only what changes, and a blanket impl makes it a
//! [`Widget`] that can be boxed as an [`Element`](super::Element) or implement further traits
//! requiring [`Widget`].
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
//! Nothing is forwarded "partly": [`children`](Widget::children) forwards too, so a host
//! traversal meets the decorator in the inner widget's place and visits every node once. Only
//! the identity methods describe the decorator itself (`concrete_type_name`,
//! [`as_any`](Widget::as_any), [`as_any_mut`](Widget::as_any_mut)). A boxed
//! [`Element`](super::Element) is wrapped with `type Inner = dyn Widget;`.
//!
//! Four consequences are worth knowing:
//!
//! * **An override replaces the inner widget's answer**, so one that adds to a collecting hook
//!   (`resources`, `search_providers`, `take_control_invocation`) includes the inner answer.
//! * **Override the host's entry point**: [`draw`](Widget::draw) and
//!   [`handle_event`](Widget::handle_event) are what the host calls, not the methods they
//!   derive from (`tooltip_target_at`, `collect_damage`).
//! * **The decorator stands in for its inner widget's layout node**, so a host reaching a
//!   widget through the node map meets the decorator, and a node the inner widget handed to a
//!   widget of its own stays that widget's. A decorator that builds its own node around the
//!   inner one registers itself with [`register_node`].
//! * **Method calls** are ambiguous with both traits imported, so implement by path (`impl
//!   ui::widget::Decorator for ..`); the prelude deliberately does not export it.

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

/// The widget a [`Decorator`] stands for, as an inventory that walks widgets
/// by their children reaches it: a decorator's inner widget is no child of
/// its own, so [`controls::control_action_bindings`] asks for it here. A
/// decorator that keeps its inner widget out of the tree — a sheet that is not
/// shown — answers nothing.
///
/// [`controls::control_action_bindings`]: crate::widget::controls::control_action_bindings
pub trait Decorated: Widget {
    /// The widget this one stands for.
    fn decorated(&self) -> &dyn Widget;
}

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

        /// The widget a decorator stands for: what an inventory walking
        /// widgets by their children asks it for its own bindings.
        impl<T: Decorator> Decorated for T {
            fn decorated(&self) -> &dyn Widget {
                self.inner().as_dyn_widget()
            }
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

        /// [`Widget`] for `$ty`, a toolkit type that
        /// implements [`concrete::Decorator`]: the blanket impl's methods, written for
        /// the one type, so they are compiled here and every app links them
        /// from the shared runtime instead of compiling its own copy.
        macro_rules! concrete_decorator_widget {
            ($d ty:ty) => {
                const _: () = {
                    use crate::widget::decorator::concrete::Decorator;

                    impl crate::widget::decorator::Decorated for $d ty {
                        fn decorated(&self) -> &dyn Widget {
                            Decorator::inner(self).as_dyn_widget()
                        }
                    }

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

        fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>)
        where
            Self: Decorated,
        {
            // What an inventory walking widgets by their children asks for the
            // widget this one stands for: its inner is no child of it. A
            // decorator that keeps its inner out of the tree overrides this to
            // answer nothing.
            if request.wants::<dyn Decorated>() {
                request.provide::<dyn Decorated>(self);
                return;
            }
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

        /// Rest what this decorator's frame drew, in the children it lays
        /// out; a decorator that adds mounted children overrides this.
        fn mark_children_drawn(&mut self) {
            crate::widget::defaults::mark_drawn(self.children_mut())
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

        fn mark_children_drawn(&mut self) {
            Decorator::mark_children_drawn(self)
        }
    }

    ref {
        fn focus_reveal_context() -> Option<vello::kurbo::Rect>;
        #[doc(hidden)]
        fn resets_binding_inheritance() -> bool;
        #[doc(hidden)]
        fn paints_ground() -> bool;
        #[doc(hidden)]
        fn inactive_bound_dispatch_boundary() -> Option<NodeId>;
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
        fn for_each_hosted_child(visitor: &mut dyn FnMut(&dyn Widget));
        fn name() -> &str;
        /// A decorator stands where its inner widget does, so the column its content is read in
        /// is the inner widget's: a bar docked under a pane around a page still finds the page.
        fn readable_column() -> Option<crate::widget::arrange::Length>;
        fn test_id() -> Option<&str>;
        fn test_rects() -> Vec<(String, vello::kurbo::Rect)>;
        fn get_rect() -> vello::kurbo::Rect;
        fn paint_bounds() -> vello::kurbo::Rect;
        fn child_clip_rect() -> Option<vello::kurbo::Rect>;
        fn own_page_title() -> Option<String>;
        fn is_item_opened_in_place() -> bool;
        fn page_title_is_heading() -> bool;
        fn snap_points(axis: crate::widget::ScrollAxis, out: &mut Vec<f32>);
        fn cursor_shape(point: vello::kurbo::Point) -> Option<winit::window::CursorIcon>;
        fn claims_raw_touch(point: vello::kurbo::Point) -> bool;
        fn touch_claim(point: vello::kurbo::Point) -> crate::interaction::TouchClaim;
        fn tooltip() -> Option<std::borrow::Cow<'_, str>>;
        fn tooltip_side() -> crate::widget::tooltip::TooltipSide;
        fn tooltip_target_at(point: vello::kurbo::Point) -> Option<(vello::kurbo::Rect, String)>;
        fn is_focusable() -> bool;
        fn is_focused() -> bool;
        fn context_menu_anchor() -> Option<vello::kurbo::Rect>;
        fn wants_tab_input() -> bool;
        fn ime_cursor_rect() -> Option<vello::kurbo::Rect>;
        fn traps_focus() -> bool;
        fn scopes_keyboard_focus() -> bool;
        fn accessibility() -> Option<crate::a11y::AccessNode>;
        fn synthetic_accessibility_rect(path: &[usize]) -> Option<vello::kurbo::Rect>;
        fn accessibility_child_parent(child_index: usize) -> Option<usize>;
        fn accessibility_child_path(child_index: usize) -> Option<Vec<usize>>;
        fn accessibility_represents_child(child_index: usize) -> bool;
        fn lays_out_child(child_index: usize) -> bool;
        fn accessibility_child_description(child_index: usize) -> Option<String>;
        fn accessibility_child_name(child_index: usize) -> Option<String>;
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
        fn preferred_window_size() -> Option<(f64, f64)>;
        fn needs_idle_sync() -> bool;
        fn wants_pointer_capture() -> bool;
        fn disclosing() -> Option<crate::widget::Disclosing>;
        fn is_band() -> bool;
        fn animation_detail() -> Option<String>;
        fn needs_animation_frame() -> bool;
        fn next_wake_deadline() -> Option<Instant>;
        fn holds_frame_until(now: Instant) -> Option<Instant>;
        fn modal_capture() -> Option<vello::kurbo::Rect>;
        fn presents_modal() -> bool;
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
            request: &mut crate::bound_dispatch::local_target::ResourceReadRequest<'_>,
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
        fn set_window_bar_control(in_bar: bool);
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
        // Not `expect`: whether clippy sees a call inside this macro's
        // expansion depends on the features the crate is built with.
        #[allow(
            clippy::disallowed_methods,
            reason = "a decorator is the widget it wraps: that widget draws under the decorator's key"
        )]
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
        fn reveal_descendant(target: *const dyn Widget, rect: vello::kurbo::Rect) -> bool;
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
        fn take_focus_request() -> Option<String>;
        fn restore_dev_route(route: &crate::widget::router::RouteSnapshot) -> bool;
        fn set_motion_reduced(reduced: bool);
        fn take_structure_dirty() -> bool;
        fn dismiss_presented_modal() -> bool;
        fn remount_children(remount: &mut crate::widget::Remount<'_>) -> bool;
        fn mark_drawn();
        fn layout_measured();
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
    }
}

// The toolkit's decorators that apps box themselves. A type here implements
// `concrete::Decorator` instead of `Decorator`.
concrete_decorator_widget!(super::nav_list::NavList);
concrete_decorator_widget!(super::media_transport::MediaTransport);
concrete_decorator_widget!(super::window_search::QueryView);
concrete_decorator_widget!(super::TabStrip);
concrete_decorator_widget!(super::page_slide::PageSlide);

#[cfg(test)]
#[path = "decorator_tests.rs"]
mod tests;
