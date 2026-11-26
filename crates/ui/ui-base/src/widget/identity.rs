//! A widget that stands for another widget without re-implementing it: the [`Decorator`] trait,
//! and [`Identified`], a control that says which action it runs however the window composes
//! around it.
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

use taffy::{NodeId, TaffyTree};
use winit::event::WindowEvent;

use super::{
    AppModel, Element, EventCtx, LayoutInfo, ManualPolicy, ModelPath, PrimaryScrollAction,
    PrimaryScrollRegion, PrimaryScrollResult, Widget, capability,
};
use crate::a11y::AccessNode;
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
    fn decorated(&self) -> &dyn Widget;
}

/// [`Decorated`], mutably: the control a decorator wraps, to change how it shows. A row that
/// gives its actions less width asks the face it fits whether it can shrink, and a wrapped one
/// answers for the control inside.
pub trait DecoratedMut: Widget {
    fn decorated_mut(&mut self) -> &mut dyn Widget;
}

/// A widget as `&dyn Widget`, also one that already is a trait object: a decorator's inner
/// widget may be either, and its layout walk hands the inner widget on
/// ([`Widget::for_each_layout_child`]).
pub trait AsDynWidget {
    fn as_dyn_widget(&self) -> &dyn Widget;
    /// This widget as a mutable trait object.
    fn as_dyn_widget_mut(&mut self) -> &mut dyn Widget;
}

impl<T: Widget> AsDynWidget for T {
    fn as_dyn_widget(&self) -> &dyn Widget {
        self
    }

    fn as_dyn_widget_mut(&mut self) -> &mut dyn Widget {
        self
    }
}

impl AsDynWidget for dyn Widget {
    fn as_dyn_widget(&self) -> &dyn Widget {
        self
    }

    fn as_dyn_widget_mut(&mut self) -> &mut dyn Widget {
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

pub fn stand_in(
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

        impl<T: Decorator> Decorated for T {
            fn decorated(&self) -> &dyn Widget {
                self.inner().as_dyn_widget()
            }
        }

        impl<T: Decorator> DecoratedMut for T {
            fn decorated_mut(&mut self) -> &mut dyn Widget {
                self.inner_mut().as_dyn_widget_mut()
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

        #[doc(hidden)]
        #[macro_export]
        macro_rules! concrete_decorator_widget {
            ([$d ($d generic:tt)*] $d ty:ty) => {
                const _: () = {
                    use ::tilo_ui_base::widget::identity::concrete::Decorator;

                    impl<$d ($d generic)*> ::tilo_ui_base::widget::identity::Decorated for $d ty {
                        fn decorated(&self) -> &dyn Widget {
                            Decorator::inner(self).as_dyn_widget()
                        }
                    }

                    impl<$d ($d generic)*> ::tilo_ui_base::widget::identity::DecoratedMut for $d ty {
                        fn decorated_mut(&mut self) -> &mut dyn Widget {
                            Decorator::inner_mut(self).as_dyn_widget_mut()
                        }
                    }

                    impl<$d ($d generic)*> Widget for $d ty {
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
            ($d ty:ty) => {
                $d crate::concrete_decorator_widget!([] $d ty);
            };
        }
    };
}

decorator_trait! {
    $
    trait {
        type Inner: Widget + AsDynWidget + ?Sized;

        fn inner(&self) -> &Self::Inner;

        fn inner_mut(&mut self) -> &mut Self::Inner;

        /// Whether what this widget shows may not be captured or streamed: the paint of it is a
        /// secret's, so a screenshot, a recording, a share and a remote viewer leave it out. A
        /// decorator standing for a view that declared `protect capture` answers true.
        fn withholds_captures(&self) -> bool {
            false
        }

        /// [`Widget::draw`] of the inner widget, in the scene part its parent opened for the
        /// decorator: a decorator is the widget it wraps, so it paints no part of its own. The
        /// blanket [`Widget`] impl notes the paint as a secret's when [`Self::withholds_captures`]
        /// says so.
        #[expect(
            clippy::disallowed_methods,
            reason = "a decorator is the widget it wraps: that widget draws under the decorator's key"
        )]
        fn draw(&mut self, ctx: &mut ::tilo_ui_base::draw::DrawCtx<'_>) {
            self.inner_mut().draw(ctx);
        }

        #[doc(hidden)]
        fn concrete_type_name(&self) -> &'static str {
            std::any::type_name::<Self>()
        }

        fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
            visitor(self.inner().as_dyn_widget());
            true
        }

        fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>)
        where
            Self: Decorated,
        {
            if request.wants::<dyn Decorated>() {
                request.provide::<dyn Decorated>(self);
                return;
            }
            self.inner().provide(request);
        }

        fn provide_mut<'a>(&'a mut self, request: &mut capability::RequestMut<'a, '_>)
        where
            Self: DecoratedMut,
        {
            if request.wants::<dyn DecoratedMut>() {
                request.provide::<dyn DecoratedMut>(self);
                return;
            }
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

        fn take_structure_change(&mut self) -> ::tilo_ui_base::widget::StructureChange {
            ::tilo_ui_base::widget::StructureChange::of(Decorator::take_structure_dirty(self))
        }

        fn composition_follows_allocation(&self) -> bool {
            ::tilo_ui_base::widget::layout::tree_composition_follows_allocation(self.inner())
        }

        /// Decorators must opt in themselves: an outer handler can change what
        /// its inner widget declares even when the inner scroll is view-only.
        fn view_only_scroll(&self, _event: &WindowEvent) -> bool {
            false
        }

        fn accessibility_hidden(&self) -> bool {
            Decorator::accessibility(self).is_some_and(|node| node.state.hidden)
        }

        fn accessibility_shared(&self) -> Option<::tilo_ui_base::a11y::SharedAccessNode> {
            None
        }

        /// The decorator's own node without its children. Not forwarded: a
        /// decorator that rewrites `accessibility` would otherwise answer
        /// with its inner widget's root. One that only adds children may
        /// forward to its inner widget's root itself.
        fn accessibility_root(&self) -> Option<::tilo_ui_base::a11y::AccessNode> {
            Decorator::accessibility(self).map(|mut node| {
                node.children.clear();
                node
            })
        }

        fn mark_children_drawn(&mut self) {
            ::tilo_ui_base::widget::defaults::mark_drawn(self.children_mut())
        }
    }

    /// Every decorator is a widget: each method is the decorator's own, which
    /// forwards to the inner widget unless the decorator overrides it.
    impl {

        #[doc(hidden)]
        fn concrete_type_name(&self) -> &'static str {
            Decorator::concrete_type_name(self)
        }

        /// [`Widget::draw`], noting the paint as a secret's when
        /// [`Decorator::withholds_captures`] says so.
        fn draw(&mut self, ctx: &mut ::tilo_ui_base::draw::DrawCtx<'_>) {
            if Decorator::withholds_captures(self) {
                ::tilo_ui_base::secret_paint::note_secret_painted();
            }
            Decorator::draw(self, ctx);
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
                Widget::concrete_type_name(self.inner()),
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

        fn take_structure_change(&mut self) -> ::tilo_ui_base::widget::StructureChange {
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

        fn accessibility_shared(&self) -> Option<::tilo_ui_base::a11y::SharedAccessNode> {
            Decorator::accessibility_shared(self)
        }

        fn accessibility_root(&self) -> Option<::tilo_ui_base::a11y::AccessNode> {
            Decorator::accessibility_root(self)
        }

        fn mark_children_drawn(&mut self) {
            Decorator::mark_children_drawn(self)
        }
    }

    ref {
        fn focus_reveal_context() -> Option<vello::kurbo::Rect>;
        fn own_actions() -> Vec<::tilo_ui_base::widget::OwnAction>;
        #[doc(hidden)]
        fn resets_binding_inheritance() -> bool;
        #[doc(hidden)]
        fn paints_ground() -> bool;
        #[doc(hidden)]
        fn inactive_bound_dispatch_boundary() -> Option<NodeId>;
        fn native_input_snapshot(state: &AppModel, path: &ModelPath) -> Option<serde_json::Value>;
        #[doc(hidden)]
        fn manual_layout_policy() -> Option<ManualPolicy>;
        fn layout_info() -> Option<LayoutInfo>;
        fn children() -> Vec<&dyn Widget>;
        fn for_each_child(visitor: &mut dyn FnMut(&dyn Widget));
        fn for_each_child_in_clip(visitor: &mut dyn FnMut(&dyn Widget));
        fn for_each_hosted_child(visitor: &mut dyn FnMut(&dyn Widget));
        fn name() -> &str;
        fn readable_column() -> Option<::tilo_ui_base::widget::Length>;
        fn test_id() -> Option<&str>;
        fn kept_in_view() -> Option<&str>;
        fn test_rects() -> Vec<(String, vello::kurbo::Rect)>;
        fn get_rect() -> vello::kurbo::Rect;
        fn paint_bounds() -> vello::kurbo::Rect;
        fn child_clip_rect() -> Option<vello::kurbo::Rect>;
        fn appears_from() -> Option<vello::kurbo::Point>;
        fn overlay_surface() -> Option<&dyn Widget>;
        fn own_page_title() -> Option<String>;
        fn is_item_opened_in_place() -> bool;
        fn page_title_is_heading() -> bool;
        fn snap_points(axis: ::tilo_ui_base::widget::ScrollAxis, out: &mut Vec<f32>);
        fn cursor_shape(point: vello::kurbo::Point) -> Option<winit::window::CursorIcon>;
        fn claims_raw_touch(point: vello::kurbo::Point) -> bool;
        fn touch_claim(point: vello::kurbo::Point) -> ::tilo_ui_base::interaction::TouchClaim;
        fn tooltip() -> Option<std::borrow::Cow<'_, str>>;
        fn tooltip_side() -> ::tilo_ui_base::widget::TooltipSide;
        fn tooltip_target_at(point: vello::kurbo::Point) -> Option<(vello::kurbo::Rect, String)>;
        fn is_focusable() -> bool;
        fn is_focused() -> bool;
        fn context_menu_anchor() -> Option<vello::kurbo::Rect>;
        fn wants_tab_input() -> bool;
        fn ime_cursor_rect() -> Option<vello::kurbo::Rect>;
        fn traps_focus() -> bool;
        fn scopes_keyboard_focus() -> bool;
        fn accessibility() -> Option<::tilo_ui_base::a11y::AccessNode>;
        fn live_picture() -> Option<String>;
        fn pressable_child() -> Option<usize>;
        fn synthetic_accessibility_rect(path: &[usize]) -> Option<vello::kurbo::Rect>;
        fn accessibility_child_parent(child_index: usize) -> Option<usize>;
        fn accessibility_child_path(child_index: usize) -> Option<Vec<usize>>;
        fn accessibility_child_node(child_index: usize) -> Option<Vec<usize>>;
        fn accessibility_represents_child(child_index: usize) -> bool;
        fn lays_out_child(child_index: usize) -> bool;
        fn accessibility_child_description(child_index: usize) -> Option<String>;
        fn accessibility_child_name(child_index: usize) -> Option<String>;
        fn child_path_name(child_index: usize) -> Option<String>;
        fn is_enabled() -> bool;
        fn ime_requires_deferred_ack() -> bool;
        fn ime_refuses_ordinary_commits() -> bool;
        fn ime_edit_fingerprint(key: &[u8; 32]) -> Option<[u8; 32]>;
        fn ime_content() -> Option<::tilo_ui_base::ime::InputContent>;
        fn ime_multiline() -> bool;
        fn ime_field() -> Option<::tilo_ui_base::ime::ImeWindowState>;
        fn presented() -> bool;
        fn arrives_from() -> ::tilo_ui_base::arriving::Source;
        fn arrival_progress() -> Option<::tilo_ui_base::arriving::Progress>;
        fn arrival_shape() -> ::tilo_ui_base::widget::SkeletonShape;
        fn text_review() -> Option<(usize, String)>;
        fn nav_availability() -> ::tilo_ui_base::gesture::NavAvailability;
        fn dev_route() -> Option<::tilo_ui_base::widget::router::RouteSnapshot>;
        fn appearance_intent() -> Option<::tilo_ui_base::theme::AppearanceIntent>;
        fn minimum_window_width() -> Option<f64>;
        fn preferred_window_size() -> Option<(f64, f64)>;
        fn needs_idle_sync() -> bool;
        fn wants_pointer_capture() -> bool;
        fn disclosing() -> Option<::tilo_ui_base::widget::Disclosing>;
        fn is_band() -> bool;
        fn keeps_its_words() -> bool;
        fn keeps_its_items() -> bool;
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
        fn is_main_answer() -> bool;
        fn primary_vertical_scroll() -> Option<PrimaryScrollRegion>;
    }

    mut {
        /// What the control inside says it does ([`::tilo_ui_base::widget::Identified`]): an outer decorator
        /// reaches it, so a window's report of what a control does moves with it.
        fn identify(identity: &str) -> bool;
        fn set_full_bleed(full_bleed: bool);
        fn place_disclosing(slot: vello::kurbo::Rect);
        fn note_placement(placement: ::tilo_ui_base::widget::Placement);
        fn retained_children_mut() -> Vec<&mut (dyn Widget + '_)>;
        fn set_platform_conventions(conventions: ::tilo_ui_base::platform::PlatformConventions);
        fn set_interaction_environment(environment: ::tilo_ui_base::interaction::InteractionEnvironment);
        fn set_window_bar_control(in_bar: bool);
        fn sync(state: &AppModel, path: &ModelPath);
        fn update(state: &AppModel, path: &ModelPath) -> bool;
        fn serve_deadline(now: Instant) -> bool;
        fn settle(edits: &mut ::tilo_ui_base::widget::Edits<'_>, path: &automerge::ObjId);
        fn measure(
            known: taffy::Size<Option<f32>>,
            available: taffy::Size<taffy::AvailableSpace>,
            text_renderer: &mut TextRenderer,
        ) -> taffy::Size<f32>;
        fn size_before_solve(taffy: &mut TaffyTree<()>, node: NodeId, text: &mut TextRenderer);
        fn style_for_width(
            styles: &mut taffy::Restyle<'_>,
            width: taffy::AvailableSpace,
            text: &mut TextRenderer,
        );
        fn style_laid_out(styles: &mut taffy::Restyle<'_>, laid: &taffy::LayoutOutput) -> bool;
        fn adjust_layout_style(
            taffy: &mut TaffyTree<()>,
            node: NodeId,
            text: &mut TextRenderer,
        ) -> bool;
        fn apply_layout(taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point);
        fn observe_physical_input(input: ::tilo_ui_base::widget::PhysicalInput);
        fn handle_event(event: &WindowEvent, ctx: &mut EventCtx<'_>);
        fn children_mut() -> Vec<&mut (dyn Widget + '_)>;
        fn reveal_descendant(target: *const dyn Widget, rect: vello::kurbo::Rect) -> bool;
        fn take_action_palette_request() -> Option<tilo_actions::ActionAddress>;
        fn take_search_request() -> bool;
        fn set_focus(focused: bool);
        fn set_invalid(invalid: bool);
        fn focus_synthetic_accessibility(path: &[usize]) -> bool;
        fn perform_action(
            action: &::tilo_ui_base::a11y::Action,
            key_path: &[u64],
            state: &mut ::tilo_ui_base::widget::Edits<'_>,
            path: &ModelPath,
        ) -> bool;
        fn perform_semantic_action(
            target: &str,
            action: &::tilo_ui_base::a11y::Action,
            state: &mut ::tilo_ui_base::widget::Edits<'_>,
            path: &ModelPath,
        ) -> bool;
        fn start_deferred_text() -> Option<tilo_webview::text_input::Receipt>;
        fn commit_deferred_text(text: &str) -> Option<tilo_webview::text_input::Receipt>;
        fn on_external_event(payload: &str, state: &mut ::tilo_ui_base::widget::Edits<'_>, root: &ModelPath);
        fn on_nav_gesture(
            dir: ::tilo_ui_base::gesture::NavDirection,
            state: &AppModel,
            path: &ModelPath,
        ) -> bool;
        fn take_focus_reset() -> bool;
        fn take_accessibility_request() -> Option<::tilo_ui_base::a11y::AccessibilityRequest>;
        #[cfg(feature = "model")]
        fn take_app_activation(ready: bool) -> Option<::tilo_ui_base::widget::AppActivation>;
        fn take_focus_request() -> Option<String>;
        fn restore_dev_route(route: &::tilo_ui_base::widget::router::RouteSnapshot) -> bool;
        fn set_motion_reduced(reduced: bool);
        fn take_structure_dirty() -> bool;
        fn dismiss_presented_modal() -> bool;
        fn remount_children(remount: &mut ::tilo_ui_base::widget::Remount<'_>) -> bool;
        fn mark_drawn();
        fn layout_measured();
        fn release_scene_cache();
        fn draw_overlay(ctx: &mut ::tilo_ui_base::draw::DrawCtx<'_>);
        fn apply_primary_vertical_scroll(action: PrimaryScrollAction) -> PrimaryScrollResult;
        fn focus_requested(target: Option<NodeId>, tree: &TaffyTree<()>);
        fn mount_native_views(host: &::tilo_ui_base::widget::native_view::NativeViewHost<'_>);
        fn native_view_frame_start();
        fn unmount_native_views();
    }
}

#[::tilo_ui_base::component::not_a_component(
    framework,
    "the host names what a view's control does"
)]
pub struct Identified {
    content: Element,
    identity: String,
}

impl Identified {
    /// `content`, a control that does `identity`: the action it runs, or the field it
    /// edits, written as the app's ids name them.
    #[must_use]
    pub fn new(content: Element, identity: impl Into<String>) -> Self {
        Self {
            content,
            identity: identity.into(),
        }
    }

    /// What the control does.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The control again, doing something else.
    pub fn identify(&mut self, identity: impl Into<String>) {
        self.identity = identity.into();
    }
}

impl Decorator for Identified {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        // What the control does is said where a press reaches it: a control that draws a button
        // of its own (an empty state's action) says it on that button, whose box a person, a
        // screen reader and an agent press. Said on the box around it, it would name a control
        // nothing answers at.
        self.content
            .accessibility()
            .map(|node| match self.content.pressable_child() {
                Some(part) => node.with_action_on_child(part, self.identity.clone()),
                None => node.with_action(self.identity.clone()),
            })
    }

    fn identify(&mut self, identity: &str) -> bool {
        Identified::identify(self, identity);
        true
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        self.content.as_any()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.content.as_any_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        self.content.concrete_type_name()
    }

    /// The content's change: an identity adds no structure of its own.
    fn take_structure_change(&mut self) -> ::tilo_ui_base::widget::StructureChange {
        self.content.take_structure_change()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &::tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.content.build_layout(taffy, theme, node_map);
        register_node(node_map, node, self);
        node
    }
}
