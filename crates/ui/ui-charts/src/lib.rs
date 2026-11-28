#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::empty_line_after_doc_comments,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::implicit_hasher,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::items_after_test_module,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_assert_eq,
    clippy::manual_find,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::needless_raw_string_hashes,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_clone,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_as_ptr,
    clippy::result_large_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unchecked_time_subtraction,
    clippy::unicode_not_nfc,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_wraps,
    clippy::unreadable_literal,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::useless_conversion,
    reason = "lint debt the charts carried in the toolkit, scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard, and tests may wait on the real clock"
    )
)]
#![doc = include_str!("../CHARTS.md")]

pub mod chart;
pub use {
    chart::Chart, chart::ChartStyle, chart::RollingSeries, chart::Series, chart::Sparkline,
    chart::TrendCurve, chart::TrendFill, chart::TrendMarks, chart::TrendRange, chart::TrendStyle,
};
pub(crate) mod chart_data;
pub(crate) mod chart_geometry;
pub(crate) mod chart_labels;
pub mod chart_readout;
pub(crate) use {
    chart_data::ChartBranch, chart_data::hierarchy, chart_data::link_description,
    chart_data::node_description, chart_data::part_description,
};
pub use {
    chart_data::ChartItem, chart_data::ChartLink, chart_data::ChartNode, chart_data::ChartPart,
    chart_data::GraphSelection,
};
pub mod chart_legend;
pub mod chart_navigation;
pub use {
    chart_navigation::ChartNavAction, chart_navigation::ChartTouchAction,
    chart_navigation::ChartTouchTracker, chart_navigation::chart_nav_action,
    chart_navigation::moved_index as move_chart_index,
    chart_navigation::moved_item as move_chart_item,
};
pub mod plot;
pub use {plot::Plot, plot::PlotFrame, plot::PlotItem, plot::PlotMarks};
pub mod cartesian_plot;
pub use {
    cartesian_plot::AxisLabelFormat, cartesian_plot::AxisRange, cartesian_plot::AxisScale,
    cartesian_plot::AxisTick, cartesian_plot::CartesianAnimationStyle,
    cartesian_plot::CartesianAnnotation, cartesian_plot::CartesianAnnotationKind,
    cartesian_plot::CartesianBrush, cartesian_plot::CartesianBrushSelection,
    cartesian_plot::CartesianGesturePolicy, cartesian_plot::CartesianPan,
    cartesian_plot::CartesianPlot, cartesian_plot::CartesianPlotProps,
    cartesian_plot::CartesianRenderingStyle, cartesian_plot::CartesianStyle,
    cartesian_plot::CartesianTooltipAnchor, cartesian_plot::CartesianTooltipStyle,
    cartesian_plot::CartesianZoom, cartesian_plot::Curve, cartesian_plot::Marker,
    cartesian_plot::MarkerShape, cartesian_plot::PlotAxis, cartesian_plot::PlotDatum,
    cartesian_plot::PlotPoints, cartesian_plot::PlotSeries, cartesian_plot::SeriesKind,
};
pub mod pie_chart;
pub use {
    pie_chart::PieChart, pie_chart::PieSlice, pie_chart::PieStyle, pie_chart::PieTooltipAnchor,
};
pub mod polar_area_chart;
pub use {polar_area_chart::PolarAreaChart, polar_area_chart::PolarAreaStyle};
pub mod heatmap;
pub use {heatmap::Heatmap, heatmap::HeatmapCell, heatmap::HeatmapStyle};
pub mod histogram;
pub use {
    histogram::Histogram, histogram::HistogramChart, histogram::HistogramNormalization,
    histogram::HistogramProps, histogram::HistogramScale,
};
pub mod candlestick_chart;
pub use {
    candlestick_chart::Candle, candlestick_chart::CandleObservation,
    candlestick_chart::CandlestickChart, candlestick_chart::CandlestickChartProps,
    candlestick_chart::CandlestickStyle,
};
pub mod chord_chart;
pub use {
    chord_chart::ChordChart, chord_chart::ChordChartProps, chord_chart::ChordSelection,
    chord_chart::ChordStyle,
};
pub mod treemap;
pub use {treemap::Treemap, treemap::TreemapProps, treemap::TreemapStyle};
pub mod radar_chart;
pub use {
    radar_chart::RadarAxis, radar_chart::RadarChart, radar_chart::RadarLegendStyle,
    radar_chart::RadarSeries, radar_chart::RadarStyle,
};
pub mod range_chart;
pub use {
    range_chart::RangeBar, range_chart::RangeChart, range_chart::RangeChartProps,
    range_chart::RangeKind, range_chart::RangeSpan, range_chart::RangeStyle,
};
pub mod category_chart;
pub use {
    category_chart::Category, category_chart::CategoryChart, category_chart::CategoryChartStyle,
    category_chart::CategoryLayout, category_chart::CategorySeries, category_chart::CategoryValue,
};
pub mod distribution_chart;
pub use {
    distribution_chart::DistributionChart, distribution_chart::DistributionKind,
    distribution_chart::DistributionSeries, distribution_chart::DistributionStyle,
};
pub mod sankey;
pub use {
    sankey::SankeyChart, sankey::SankeyChartProps, sankey::SankeySelection, sankey::SankeyStyle,
};
pub mod sunburst;
pub use {
    sunburst::SunburstChart, sunburst::SunburstChartProps, sunburst::SunburstNode,
    sunburst::SunburstStyle,
};
pub mod funnel_chart;
pub use {funnel_chart::FunnelChart, funnel_chart::FunnelDirection, funnel_chart::FunnelStyle};
pub mod function_plane;
pub use function_plane::FunctionPlane;
pub mod stat_tile;
pub use function_plane::{format_tick, nice_step, nice_ticks};
pub use {
    stat_tile::StatGroup, stat_tile::StatGroupProps, stat_tile::StatTile, stat_tile::StatTileFace,
    stat_tile::StatTileProps, stat_tile::Trend,
};

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The charts a window builds by name, beside the toolkit's (`tilo_ui::component_catalogue::TOOLKIT`).
pub static COMPONENTS: tilo_ui_base::component::Set = tilo_ui_base::component::Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: chart_enums,
};

fn chart_enums() -> Vec<tilo_ui_base::component::EnumDescription> {
    vec![
        chart::series_description(),
        category_chart::bars_description(),
        pie_chart::slice_description(),
        distribution_chart::samples_description(),
        polar_area_chart::areas_description(),
        radar_chart::axes_description(),
        radar_chart::radar_series_description(),
        funnel_chart::stages_description(),
        heatmap::cells_description(),
        part_description(),
        node_description(),
        link_description(),
        cartesian_plot::points_description(),
        candlestick_chart::candle_description(),
        range_chart::range_description(),
    ]
}
