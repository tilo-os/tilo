//! The toolkit's own widgets that stand for another widget they wrap.
//!
//! Each is written as a [`concrete::Decorator`](tilo_ui_base::widget::identity::concrete::Decorator)
//! beside the widget itself; the widget's [`Widget`](tilo_ui_base::widget::Widget) glue, which forwards
//! only what the widget overrides, is generated here, where the widget types are in scope, so it
//! is compiled once into the toolkit rather than in every app that boxes one.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use winit::event::WindowEvent;

use crate::time::Instant;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{
    widget::AppModel, widget::EventCtx, widget::LayoutInfo, widget::ManualPolicy,
    widget::ModelPath, widget::PrimaryScrollAction, widget::PrimaryScrollRegion,
    widget::PrimaryScrollResult, widget::Widget, widget::capability,
};
use tilo_ui_base::{widget::identity::AsDynWidget, widget::identity::stand_in};

tilo_ui_base::concrete_decorator_widget!(super::nav_list::NavList);
tilo_ui_base::concrete_decorator_widget!(super::media_transport::MediaTransport);
tilo_ui_base::concrete_decorator_widget!(super::window_search::QueryView);
tilo_ui_base::concrete_decorator_widget!(super::TabStrip);
tilo_ui_base::concrete_decorator_widget!(super::page_slide::PageSlide);
tilo_ui_base::concrete_decorator_widget!(super::fullscreen_view::Fullscreen);
tilo_ui_base::concrete_decorator_widget!([M: super::ListModel + 'static] super::VirtualList<M>);
