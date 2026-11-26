//! The toolkit's own described widgets as a [`Set`] a window registers: every widget this crate
//! declares (`impl Described`) and every component that stands only in a part of another, generated
//! from the sources, and the closed sets with fields their props name.
//!
//! The catalogue names the widgets themselves, so it sits above the description machinery
//! ([`tilo_ui_base::component`]): the machinery describes a component, and this lists the ones the
//! toolkit has.

use tilo_ui_base::{component::EnumDescription, component::Set};

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The toolkit's own described widgets.
pub static TOOLKIT: Set = Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: toolkit_enums,
};

fn toolkit_enums() -> Vec<EnumDescription> {
    vec![
        crate::widget::drawing::shape_description(),
        crate::widget::key_pad::key_pad_description(),
        crate::widget::chart::series_description(),
        crate::widget::connection_description(),
        crate::widget::category_chart::bars_description(),
        crate::widget::pie_chart::slice_description(),
        crate::widget::distribution_chart::samples_description(),
        crate::widget::polar_area_chart::areas_description(),
        crate::widget::radar_chart::axes_description(),
        crate::widget::radar_chart::radar_series_description(),
        crate::widget::funnel_chart::stages_description(),
        crate::widget::sheet_component::sheet_cell_description(),
        crate::widget::sheet_component::sheet_peer_description(),
        crate::widget::sheet_component::sheet_size_description(),
        crate::widget::heatmap::cells_description(),
        crate::widget::part_description(),
        crate::widget::node_description(),
        crate::widget::link_description(),
        crate::widget::cartesian_plot::points_description(),
        crate::widget::candlestick_chart::candle_description(),
        crate::widget::range_chart::range_description(),
    ]
}
