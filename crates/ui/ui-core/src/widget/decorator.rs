//! The toolkit's own widgets that stand for another widget they wrap.
//!
//! Each is written as a [`concrete::Decorator`](crate::widget::identity::concrete::Decorator)
//! beside the widget itself; the widget's [`Widget`](crate::widget::Widget) glue, which forwards
//! only what the widget overrides, is generated here, where the widget types are in scope, so it
//! is compiled once into the toolkit rather than in every app that boxes one.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use winit::event::WindowEvent;

use super::identity::{AsDynWidget, concrete_decorator_widget, stand_in};
use super::{
    AppModel, EventCtx, LayoutInfo, ManualPolicy, ModelPath, PrimaryScrollAction,
    PrimaryScrollRegion, PrimaryScrollResult, Widget, capability,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;

concrete_decorator_widget!(super::nav_list::NavList);
concrete_decorator_widget!(super::media_transport::MediaTransport);
concrete_decorator_widget!(super::window_search::QueryView);
concrete_decorator_widget!(super::TabStrip);
concrete_decorator_widget!(super::page_slide::PageSlide);
concrete_decorator_widget!(super::fullscreen_view::Fullscreen);
concrete_decorator_widget!([M: super::ListModel + 'static] super::VirtualList<M>);
